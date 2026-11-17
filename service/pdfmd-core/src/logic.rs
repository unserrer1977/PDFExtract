//! Decide how a PdfCraft extract becomes the Markdown document callers receive.

/// The largest PDF the service will accept, in bytes.
pub const MAX_PDF_BYTES: usize = 32 * 1024 * 1024;

pub const IMAGE_NOTICE: &str = "This file looks like a scan or a set of pictures. PdfCraft did not find a text layer, so words that exist only inside images are not in this document.";

/// What PdfCraft returned before this service adds a title and any notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extract {
    pub title: Option<String>,
    pub pages: usize,
    pub truncated_pages: Vec<usize>,
    pub markdown: String,
}

/// A PDF the converter should read.
#[derive(Debug, Clone)]
pub struct OwnedPdf {
    pub name: String,
    pub bytes: Vec<u8>,
    pub password: Option<String>,
}

/// Failure from opening a PDF or fetching one. `status` is an HTTP status code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertFailure {
    pub status: u16,
    pub message: String,
}

impl ConvertFailure {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
}

/// JSON body of a successful conversion.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Conversion {
    pub markdown: String,
    pub title: Option<String>,
    pub pages: usize,
    /// Zero-based page indexes PdfCraft could not read completely.
    pub truncated_pages: Vec<usize>,
    pub image_based: bool,
    pub notice: Option<String>,
}

/// PdfCraft found pages, but almost no letters, and the file is pictures or has no text.
pub fn is_image_based(page_count: usize, letters: usize, has_image: bool) -> bool {
    if page_count == 0 {
        return false;
    }
    let limit = page_count.saturating_mul(20).max(20);
    if letters >= limit {
        return false;
    }
    has_image || letters == 0
}

pub fn letter_count(text: &str) -> usize {
    text.chars().filter(|c| c.is_alphanumeric()).count()
}

/// True when the PDF names an image XObject. This does not decode the pictures.
pub fn pdf_declares_image(bytes: &[u8]) -> bool {
    contains(bytes, b"/Subtype /Image")
        || contains(bytes, b"/Subtype/Image")
        || contains(bytes, b"/ImageMask")
}

pub fn looks_like_pdf(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    contains(head, b"%PDF")
}

pub fn compose(extract: Extract, source: &[u8]) -> Conversion {
    let body = extract.markdown.trim_start_matches('\u{feff}').trim();
    let letters = letter_count(body);
    let image_based = is_image_based(extract.pages, letters, pdf_declares_image(source));
    let mut markdown = String::new();
    if image_based {
        markdown.push_str("> **Image-based PDF.** ");
        markdown.push_str(IMAGE_NOTICE);
        markdown.push_str("\n\n");
    }
    if let Some(title) = extract.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if !starts_with_title(body, title) {
            markdown.push_str("# ");
            markdown.push_str(&single_line(title));
            markdown.push_str("\n\n");
        }
    }
    markdown.push_str(body);
    if !extract.truncated_pages.is_empty() {
        if !markdown.ends_with('\n') {
            markdown.push('\n');
        }
        markdown.push('\n');
        markdown.push_str(&truncation_note(&extract.truncated_pages));
        markdown.push('\n');
    } else if !markdown.is_empty() && !markdown.ends_with('\n') {
        markdown.push('\n');
    }
    Conversion {
        markdown,
        title: extract.title,
        pages: extract.pages,
        truncated_pages: extract.truncated_pages,
        image_based,
        notice: image_based.then(|| IMAGE_NOTICE.to_string()),
    }
}

fn truncation_note(pages: &[usize]) -> String {
    let labels: Vec<String> = pages.iter().map(|page| (page + 1).to_string()).collect();
    let list = labels.join(", ");
    if pages.len() == 1 {
        format!("> **Incomplete extract.** PdfCraft stopped early on page {list}. Some text on that page is missing.")
    } else {
        format!("> **Incomplete extract.** PdfCraft stopped early on pages {list}. Some text on those pages is missing.")
    }
}

fn starts_with_title(markdown: &str, title: &str) -> bool {
    let wanted = normalize(title);
    if wanted.is_empty() {
        return false;
    }
    for line in markdown.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix('#') else {
            return false;
        };
        let heading = rest.trim_start_matches('#').trim();
        return normalize(heading) == wanted;
    }
    false
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(markdown: &str) -> Extract {
        Extract {
            title: Some("Quarterly notes".into()),
            pages: 1,
            truncated_pages: Vec::new(),
            markdown: markdown.into(),
        }
    }

    #[test]
    fn text_pdf_keeps_structure_and_adds_a_title() {
        let source = b"%PDF-1.7\n1 0 obj\n<< /Type /Page >>\nendobj\n";
        let conversion = compose(extract("## Findings\n\nRevenue rose.\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n"), source);
        assert!(!conversion.image_based);
        assert!(conversion.notice.is_none());
        assert!(conversion.markdown.starts_with("# Quarterly notes\n"));
        assert!(conversion.markdown.contains("## Findings"));
        assert!(conversion.markdown.contains("| A | B |"));
        assert!(!conversion.markdown.contains("!["));
    }

    #[test]
    fn existing_title_heading_is_not_repeated() {
        let conversion = compose(extract("# Quarterly notes\n\nBody text that is long enough to count as a real text layer.\n"), b"%PDF");
        assert_eq!(conversion.markdown.matches("# Quarterly notes").count(), 1);
    }

    #[test]
    fn image_only_pdf_carries_the_notice() {
        let source = b"%PDF-1.4\n/Subtype /Image\n";
        let conversion = compose(
            Extract { title: Some("Scan".into()), pages: 2, truncated_pages: vec![1], markdown: String::new() },
            source,
        );
        assert!(conversion.image_based);
        assert_eq!(conversion.notice.as_deref(), Some(IMAGE_NOTICE));
        assert!(conversion.markdown.starts_with("> **Image-based PDF.** "));
        assert!(conversion.markdown.contains(IMAGE_NOTICE));
        assert!(conversion.markdown.contains("# Scan"));
        assert!(conversion.markdown.contains("page 2"));
        assert!(!conversion.markdown.contains("!["));
    }

    #[test]
    fn a_short_text_page_without_pictures_is_not_called_a_scan() {
        assert!(!is_image_based(1, 12, false));
        assert!(is_image_based(1, 12, true));
        assert!(is_image_based(1, 0, false));
        assert!(!is_image_based(1, 40, true));
    }
}
