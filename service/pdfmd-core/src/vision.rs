//! Transcribe image-only PDFs the way AngelOS does.
//!
//! PdfCraft stays the reader when a text layer exists. When it does not, pages go to
//! OpenRouter in batches of five, with the same auto-router and file-parser settings
//! AngelOS uses for pitch decks.

use std::time::Duration;

use base64::Engine;
use futures_util::future::BoxFuture;

use crate::logic::{apply_vision, note_vision_failure, Conversion};

const OCR_BATCH_PAGES: usize = 5;
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const BATCH_TIMEOUT: Duration = Duration::from_secs(60);

/// What the image reader did for one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisionOutcome {
    /// No OpenRouter key is configured, so the scan notice stands.
    Skipped,
    /// Page images were transcribed.
    Recovered(String),
    /// The reader was configured and did not return text.
    Failed,
}

pub trait VisionOcr: Send + Sync + 'static {
    fn transcribe<'a>(&'a self, pdf: &'a [u8], pages: usize) -> BoxFuture<'a, VisionOutcome>;
}

pub struct OpenRouterVision {
    api_key: String,
    client: reqwest::Client,
}

impl OpenRouterVision {
    pub fn from_env() -> Self {
        let api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
        let client = reqwest::Client::builder().build().unwrap_or_else(|_| reqwest::Client::new());
        Self { api_key, client }
    }
}

impl VisionOcr for OpenRouterVision {
    fn transcribe<'a>(&'a self, pdf: &'a [u8], pages: usize) -> BoxFuture<'a, VisionOutcome> {
        Box::pin(async move {
            if self.api_key.trim().is_empty() {
                return VisionOutcome::Skipped;
            }
            match transcribe_batches(&self.client, &self.api_key, pdf, pages).await {
                Ok(markdown) if !markdown.trim().is_empty() => VisionOutcome::Recovered(markdown),
                _ => VisionOutcome::Failed,
            }
        })
    }
}

pub fn finish_vision(conversion: Conversion, outcome: VisionOutcome) -> Conversion {
    match outcome {
        VisionOutcome::Skipped => conversion,
        VisionOutcome::Recovered(markdown) => apply_vision(conversion, &markdown),
        VisionOutcome::Failed => note_vision_failure(conversion),
    }
}

async fn transcribe_batches(client: &reqwest::Client, api_key: &str, pdf: &[u8], pages: usize) -> Result<String, String> {
    let mut found: Vec<(u32, String)> = Vec::new();
    for (batch, bytes) in pdf_batches(pdf, pages) {
        let Ok(reply) = request_batch(client, api_key, &batch, &bytes).await else {
            continue;
        };
        found.extend(reply_for_batch(&reply, &batch));
    }
    found.sort_by_key(|(page, _)| *page);
    found.dedup_by_key(|(page, _)| *page);
    if found.is_empty() {
        return Err("image reader returned no text".into());
    }
    Ok(pages_to_markdown(&found))
}

async fn request_batch(client: &reqwest::Client, api_key: &str, batch: &[u32], pdf: &[u8]) -> Result<String, String> {
    let pages = batch.iter().map(|page| page.to_string()).collect::<Vec<_>>().join(", ");
    let prompt = format!(
        "This PDF holds pitch-deck pages {pages} in order. Transcribe all visible text, numbers and chart/table values on each page. Output only blocks headed \"=== Page N ===\" using those original page numbers."
    );
    let payload = serde_json::json!({
        "model": "openrouter/auto",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "file", "file": {
                    "filename": "deck-pages.pdf",
                    "file_data": format!("data:application/pdf;base64,{}", base64::engine::general_purpose::STANDARD.encode(pdf))
                }}
            ]
        }],
        "plugins": [
            {"id": "auto-router", "cost_tier": "low"},
            {"id": "file-parser", "pdf": {"engine": "cloudflare-ai"}}
        ],
        "provider": {"sort": "price"},
        "usage": {"include": true}
    });
    let response = client
        .post(OPENROUTER_URL)
        .timeout(BATCH_TIMEOUT)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("HTTP-Referer", "https://angelos.vc")
        .header("X-OpenRouter-Title", "AngelOS")
        .body(serde_json::to_vec(&payload).map_err(|err| err.to_string())?)
        .send()
        .await
        .map_err(|err| format!("image reader request failed: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("image reader returned HTTP {}", response.status()));
    }
    let bytes = response.bytes().await.map_err(|err| format!("image reader reply could not be read: {err}"))?;
    let body: serde_json::Value = serde_json::from_slice(&bytes).map_err(|err| format!("image reader reply was not JSON: {err}"))?;
    Ok(message_text(&body["choices"][0]["message"]["content"]))
}

fn message_text(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits an OCR reply of "=== Page N ===" blocks into page numbers and text.
pub fn parse_ocr_reply(reply: &str) -> Vec<(u32, String)> {
    let headings = page_headings(reply);
    let mut pages = Vec::new();
    for (index, heading) in headings.iter().enumerate() {
        let end = headings.get(index + 1).map(|next| next.marker).unwrap_or(reply.len());
        let text = normalise(&reply[heading.body..end]);
        if !text.is_empty() {
            pages.push((heading.number, text));
        }
    }
    pages
}

struct PageHeading {
    number: u32,
    marker: usize,
    body: usize,
}

fn page_headings(text: &str) -> Vec<PageHeading> {
    let lower = text.to_ascii_lowercase();
    let marker = "=== page ";
    let mut search = 0;
    let mut headings = Vec::new();
    while let Some(relative) = lower[search..].find(marker) {
        let at = search + relative;
        let after = &text[at + marker.len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            search = at + marker.len();
            continue;
        }
        let Ok(number) = digits.parse::<u32>() else {
            search = at + marker.len();
            continue;
        };
        let after_digits = &after[digits.len()..];
        let Some(close) = after_digits.find("===") else {
            search = at + marker.len();
            continue;
        };
        let body = at + marker.len() + digits.len() + close + 3;
        headings.push(PageHeading { number, marker: at, body });
        search = body;
    }
    headings
}

fn reply_for_batch(reply: &str, batch: &[u32]) -> Vec<(u32, String)> {
    let parsed = parse_ocr_reply(reply);
    if parsed.is_empty() && batch.len() == 1 {
        let text = normalise(reply);
        if text.is_empty() {
            Vec::new()
        } else {
            vec![(batch[0], text)]
        }
    } else {
        parsed
    }
}

pub fn pages_to_markdown(pages: &[(u32, String)]) -> String {
    let mut markdown = String::new();
    for (page, text) in pages {
        markdown.push_str(&format!("## Page {page}\n\n{}\n\n", text.trim()));
    }
    markdown
}

fn normalise(text: &str) -> String {
    let mut collapsed = String::new();
    let mut pending_space = false;
    let mut newlines = 0;
    for character in text.chars() {
        if character == '\n' {
            pending_space = false;
            newlines += 1;
            continue;
        }
        if character == ' ' || character == '\t' {
            if newlines == 0 {
                pending_space = !collapsed.is_empty();
            }
            continue;
        }
        if newlines > 0 {
            let count = newlines.min(2);
            for _ in 0..count {
                collapsed.push('\n');
            }
            newlines = 0;
            pending_space = false;
        } else if pending_space {
            collapsed.push(' ');
            pending_space = false;
        }
        collapsed.push(character);
    }
    collapsed.trim().to_string()
}

fn pdf_batches(bytes: &[u8], pages: usize) -> Vec<(Vec<u32>, Vec<u8>)> {
    match split_batches(bytes, pages) {
        Ok(batches) if !batches.is_empty() => batches,
        _ => {
            let numbers = (1..=pages.max(1) as u32).collect();
            vec![(numbers, bytes.to_vec())]
        }
    }
}

fn split_batches(bytes: &[u8], fallback_pages: usize) -> Result<Vec<(Vec<u32>, Vec<u8>)>, String> {
    let doc = lopdf::Document::load_mem(bytes).map_err(|err| err.to_string())?;
    let mut numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
    if numbers.is_empty() {
        numbers = (1..=fallback_pages.max(1) as u32).collect();
    }
    let mut batches = Vec::new();
    for batch in numbers.chunks(OCR_BATCH_PAGES) {
        if batch.len() == numbers.len() {
            batches.push((batch.to_vec(), bytes.to_vec()));
            continue;
        }
        let mut copy = lopdf::Document::load_mem(bytes).map_err(|err| err.to_string())?;
        let present: Vec<u32> = copy.get_pages().keys().copied().collect();
        let drop: Vec<u32> = present.into_iter().filter(|page| !batch.contains(page)).collect();
        if !drop.is_empty() {
            copy.delete_pages(&drop);
        }
        let mut saved = Vec::new();
        copy.save_to(&mut saved).map_err(|err| err.to_string())?;
        batches.push((batch.to_vec(), saved));
    }
    Ok(batches)
}

/// Lets tests choose the image-reader result and see whether it was asked.
pub struct ScriptedVision {
    pub outcome: VisionOutcome,
    pub calls: std::sync::atomic::AtomicUsize,
}

impl VisionOcr for ScriptedVision {
    fn transcribe<'a>(&'a self, _pdf: &'a [u8], _pages: usize) -> BoxFuture<'a, VisionOutcome> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_blocks_keep_their_numbers() {
        let reply = "=== Page 2 ===\nRevenue rose\n\n=== Page 4 ===\nTeam of 12";
        assert_eq!(
            parse_ocr_reply(reply),
            vec![(2, "Revenue rose".into()), (4, "Team of 12".into())]
        );
    }

    #[test]
    fn a_single_page_reply_without_a_heading_still_counts() {
        assert_eq!(reply_for_batch("Hello from the slide", &[3]), vec![(3, "Hello from the slide".into())]);
    }

    #[test]
    fn markdown_uses_page_headings() {
        let markdown = pages_to_markdown(&[(1, "Alpha".into()), (2, "Beta".into())]);
        assert!(markdown.contains("## Page 1\n\nAlpha"));
        assert!(markdown.contains("## Page 2\n\nBeta"));
    }
}
