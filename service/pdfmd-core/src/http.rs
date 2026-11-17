//! Routes other applications call, and the page a person can use in a browser.

use std::sync::Arc;

use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;
use tower_http::limit::RequestBodyLimitLayer;

use crate::logic::{compose, looks_like_pdf, ConvertFailure, Conversion, Extract, OwnedPdf, MAX_PDF_BYTES};
use crate::openapi::OPENAPI;
use crate::ssrf::fetch_pdf;
use crate::vision::{finish_vision, OpenRouterVision, VisionOcr};

const INDEX: &str = include_str!("../templates/index.html");

/// Reads PDF bytes into an extract. Implemented with PdfCraft in the server binary.
pub trait PdfConvert: Send + Sync + 'static {
    fn convert(&self, input: OwnedPdf) -> Result<Extract, ConvertFailure>;
}

pub struct AppState<C> {
    pub api_key: String,
    pub converter: Arc<C>,
    pub slots: Arc<Semaphore>,
    pub vision: Arc<dyn VisionOcr>,
}

impl<C> Clone for AppState<C> {
    fn clone(&self) -> Self {
        Self {
            api_key: self.api_key.clone(),
            converter: Arc::clone(&self.converter),
            slots: Arc::clone(&self.slots),
            vision: Arc::clone(&self.vision),
        }
    }
}

pub fn router<C: PdfConvert>(state: AppState<C>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route("/v1/convert", post(convert))
        .layer(RequestBodyLimitLayer::new(MAX_PDF_BYTES + 1024 * 1024))
        .with_state(state)
}

pub fn boot<C: PdfConvert>(converter: C) -> Result<(), String> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let port: u16 = port.parse().map_err(|_| format!("PORT is not a valid port: {port}"))?;
    let api_key = std::env::var("PDFEXTRACT_API_KEY").unwrap_or_default();
    let state = AppState {
        api_key,
        converter: Arc::new(converter),
        slots: Arc::new(Semaphore::new(1)),
        vision: Arc::new(OpenRouterVision::from_env()),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|err| err.to_string())?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await.map_err(|err| err.to_string())?;
        println!("pdfextract listening on 0.0.0.0:{port}");
        axum::serve(listener, router(state)).await.map_err(|err| err.to_string())
    })
}

async fn index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX)
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn openapi() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], OPENAPI)
}

#[derive(Debug, Deserialize)]
struct FormatQuery {
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UrlRequest {
    url: String,
    password: Option<String>,
}

async fn convert<C: PdfConvert>(State(state): State<AppState<C>>, Query(query): Query<FormatQuery>, request: Request) -> Response {
    if let Some(response) = authorize(&state.api_key, request.headers()) {
        return response;
    }
    let markdown = wants_markdown(request.headers(), query.format.as_deref());
    let content_type = request.headers().get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or("").to_string();
    let input = if content_type.starts_with("application/json") {
        match json_input(request).await {
            Ok(input) => input,
            Err(response) => return response,
        }
    } else if content_type.starts_with("multipart/") {
        match multipart_input(request, &content_type).await {
            Ok(input) => input,
            Err(response) => return response,
        }
    } else {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Send the PDF as multipart form data or JSON with a url");
    };
    let permit = match state.slots.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Conversion is unavailable"),
    };
    let source = input.bytes.clone();
    let converter = Arc::clone(&state.converter);
    let joined = tokio::task::spawn_blocking(move || process(converter.as_ref(), input)).await;
    let conversion = match joined {
        Ok(Ok(conversion)) => conversion,
        Ok(Err(failure)) => return error(status_code(failure.status), failure.message),
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "Conversion task stopped"),
    };
    let conversion = if conversion.image_based {
        let outcome = state.vision.transcribe(&source, conversion.pages).await;
        finish_vision(conversion, outcome)
    } else {
        conversion
    };
    drop(permit);
    respond(conversion, markdown)
}

fn process<C: PdfConvert>(converter: &C, input: OwnedPdf) -> Result<Conversion, ConvertFailure> {
    let source = input.bytes.clone();
    let extract = converter.convert(input)?;
    if extract.pages == 0 {
        return Err(ConvertFailure::new(422, "PdfCraft did not find any pages in this PDF"));
    }
    Ok(compose(extract, &source))
}

async fn json_input(request: Request) -> Result<OwnedPdf, Response> {
    let bytes = read_body(request).await?;
    let payload: UrlRequest = serde_json::from_slice(&bytes).map_err(|_| error(StatusCode::BAD_REQUEST, "JSON body must include a url"))?;
    let fetched = fetch_pdf(&payload.url).await.map_err(|message| error(status_for_fetch(&message), message))?;
    Ok(OwnedPdf {
        name: name_from_url(&payload.url),
        bytes: fetched,
        password: blank_to_none(payload.password),
    })
}

async fn multipart_input(request: Request, content_type: &str) -> Result<OwnedPdf, Response> {
    let boundary = multer::parse_boundary(content_type).map_err(|_| error(StatusCode::BAD_REQUEST, "Multipart body has no boundary"))?;
    let stream = request.into_body().into_data_stream().map(|item| item.map_err(std::io::Error::other));
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut file: Option<(String, Vec<u8>)> = None;
    let mut url: Option<String> = None;
    let mut password: Option<String> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => return Err(error(StatusCode::BAD_REQUEST, format!("Could not read the upload: {err}"))),
        };
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                let filename = field.file_name().map(sanitize_name).unwrap_or_else(|| "document.pdf".into());
                let data = field.bytes().await.map_err(|err| error(StatusCode::BAD_REQUEST, format!("Could not read the upload: {err}")))?;
                if data.len() > MAX_PDF_BYTES {
                    return Err(error(StatusCode::PAYLOAD_TOO_LARGE, format!("PDF is larger than {} MB", MAX_PDF_BYTES / (1024 * 1024))));
                }
                file = Some((filename, data.to_vec()));
            }
            "url" => {
                url = Some(field.text().await.map_err(|err| error(StatusCode::BAD_REQUEST, format!("Could not read the URL: {err}")))?);
            }
            "password" => {
                password = Some(field.text().await.map_err(|err| error(StatusCode::BAD_REQUEST, format!("Could not read the password: {err}")))?);
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }
    let password = blank_to_none(password);
    match (file, url.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())) {
        (Some(_), Some(_)) => Err(error(StatusCode::BAD_REQUEST, "Send a file or a URL, not both")),
        (Some((name, bytes)), None) => {
            if !looks_like_pdf(&bytes) {
                return Err(error(StatusCode::BAD_REQUEST, "The upload is not a PDF"));
            }
            Ok(OwnedPdf { name, bytes, password })
        }
        (None, Some(url)) => {
            let bytes = fetch_pdf(&url).await.map_err(|message| error(status_for_fetch(&message), message))?;
            Ok(OwnedPdf { name: name_from_url(&url), bytes, password })
        }
        (None, None) => Err(error(StatusCode::BAD_REQUEST, "Choose a PDF file or a URL")),
    }
}

async fn read_body(request: Request) -> Result<bytes::Bytes, Response> {
    use http_body_util::BodyExt;
    let collected = request.into_body().collect().await.map_err(|_| error(StatusCode::BAD_REQUEST, "Could not read the request body"))?;
    let bytes = collected.to_bytes();
    if bytes.len() > MAX_PDF_BYTES {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, format!("PDF is larger than {} MB", MAX_PDF_BYTES / (1024 * 1024))));
    }
    Ok(bytes)
}

fn authorize(api_key: &str, headers: &HeaderMap) -> Option<Response> {
    if api_key.is_empty() {
        return Some(error(StatusCode::SERVICE_UNAVAILABLE, "PDFEXTRACT_API_KEY is not configured"));
    }
    let provided = bearer(headers).unwrap_or("");
    if keys_match(api_key, provided) {
        None
    } else {
        Some(error(StatusCode::UNAUTHORIZED, "Unauthorized"))
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))?;
    let token = rest.trim();
    if token.is_empty() { None } else { Some(token) }
}

pub fn keys_match(expected: &str, provided: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let a = expected.as_bytes();
    let b = provided.as_bytes();
    let mut diff = a.len() ^ b.len();
    let len = a.len().max(b.len());
    let mut index = 0;
    while index < len {
        let left = if index < a.len() { a[index] } else { 0 };
        let right = if index < b.len() { b[index] } else { 0 };
        diff |= usize::from(left ^ right);
        index += 1;
    }
    diff == 0
}

fn wants_markdown(headers: &HeaderMap, format: Option<&str>) -> bool {
    if let Some(format) = format {
        return format.eq_ignore_ascii_case("markdown") || format.eq_ignore_ascii_case("md");
    }
    headers.get(header::ACCEPT).and_then(|value| value.to_str().ok()).map(|value| value.contains("text/markdown")).unwrap_or(false)
}

fn respond(conversion: Conversion, markdown: bool) -> Response {
    let image_based = if conversion.image_based { "true" } else { "false" };
    let mut response = if markdown {
        (StatusCode::OK, [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], conversion.markdown).into_response()
    } else {
        Json(conversion).into_response()
    };
    response.headers_mut().insert(header::HeaderName::from_static("x-image-based"), header::HeaderValue::from_static(image_based));
    response
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

fn status_code(status: u16) -> StatusCode {
    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

fn status_for_fetch(message: &str) -> StatusCode {
    if message.contains("larger than") { StatusCode::PAYLOAD_TOO_LARGE } else { StatusCode::BAD_REQUEST }
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let text = text.trim().to_string();
        if text.is_empty() { None } else { Some(text) }
    })
}

fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    if cleaned.is_empty() { "document.pdf".into() } else { cleaned }
}

fn name_from_url(raw: &str) -> String {
    url::Url::parse(raw)
        .ok()
        .and_then(|url| url.path_segments().and_then(|mut parts| parts.next_back()).map(sanitize_name))
        .filter(|name| name.to_ascii_lowercase().ends_with(".pdf"))
        .unwrap_or_else(|| "document.pdf".into())
}

use futures_util::StreamExt;
