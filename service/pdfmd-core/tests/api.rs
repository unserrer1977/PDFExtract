use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pdfmd_core::{router, AppState, ConvertFailure, Extract, OwnedPdf, PdfConvert, IMAGE_NOTICE};
use serde_json::Value;
use tokio::sync::Semaphore;
use tower::ServiceExt;

struct Fake;

impl PdfConvert for Fake {
    fn convert(&self, input: OwnedPdf) -> Result<Extract, ConvertFailure> {
        if input.bytes.windows(b"/Subtype /Image".len()).any(|window| window == b"/Subtype /Image") {
            return Ok(Extract { title: Some("Scan".into()), pages: 1, truncated_pages: Vec::new(), markdown: String::new() });
        }
        if input.password.as_deref() == Some("wrong") {
            return Err(ConvertFailure::new(400, "the password is incorrect"));
        }
        Ok(Extract {
            title: Some("Notes".into()),
            pages: 2,
            truncated_pages: Vec::new(),
            markdown: "## Section\n\nRevenue rose in the quarter and the table below is the split.\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n".into(),
        })
    }
}

fn app() -> axum::Router {
    router(AppState {
        api_key: "test-key".into(),
        converter: Arc::new(Fake),
        slots: Arc::new(Semaphore::new(1)),
    })
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn health_and_spec_do_not_need_a_key() {
    let health = app().oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let spec = app().oneshot(Request::builder().uri("/openapi.json").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(spec.status(), StatusCode::OK);
    let page = app().oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
}

#[tokio::test]
async fn convert_requires_the_api_key() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/convert")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://example.com/a.pdf"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn upload_returns_structured_markdown_without_images() {
    let pdf = b"%PDF-1.4\nHello from the text layer\n".to_vec();
    let boundary = "pdfextract";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"notes.pdf\"\r\nContent-Type: application/pdf\r\n\r\n").as_bytes());
    body.extend_from_slice(&pdf);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/convert")
                .header("authorization", "Bearer test-key")
                .header("content-type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-image-based").unwrap(), "false");
    let payload = body_json(response).await;
    let markdown = payload["markdown"].as_str().unwrap();
    assert!(markdown.contains("# Notes"));
    assert!(markdown.contains("## Section"));
    assert!(markdown.contains("| A | B |"));
    assert!(!markdown.contains("!["));
    assert!(payload["image_based"].as_bool() == Some(false));
    assert!(payload["notice"].is_null());
}

#[tokio::test]
async fn image_pdf_is_reported_in_json_and_markdown() {
    let pdf = b"%PDF-1.4\n/Subtype /Image\n".to_vec();
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/convert?format=markdown")
                .header("authorization", "Bearer test-key")
                .header("content-type", "multipart/form-data; boundary=edge")
                .body(Body::from(format!(
                    "--edge\r\nContent-Disposition: form-data; name=\"file\"; filename=\"scan.pdf\"\r\nContent-Type: application/pdf\r\n\r\n{}\r\n--edge--\r\n",
                    String::from_utf8(pdf).unwrap()
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-image-based").unwrap(), "true");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let markdown = std::str::from_utf8(&bytes).unwrap();
    assert!(markdown.contains(IMAGE_NOTICE));
    assert!(!markdown.contains("!["));
}

#[tokio::test]
async fn url_targets_on_the_private_network_are_rejected() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/convert")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"http://127.0.0.1/secret.pdf"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = body_json(response).await;
    assert!(payload["error"].as_str().unwrap().contains("public"));
}

#[tokio::test]
async fn an_unconfigured_key_is_not_an_open_service() {
    let open = router(AppState {
        api_key: String::new(),
        converter: Arc::new(Fake),
        slots: Arc::new(Semaphore::new(1)),
    });
    let response = open
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/convert")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://8.8.8.8/a.pdf"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
