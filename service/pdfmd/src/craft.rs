//! PdfCraft is the only PDF reader this service uses.
//!
//! `Document::export_office_checked(OfficeFormat::Markdown)` is PdfCraft's Markdown
//! export. That writer keeps reading order, headings, emphasis and tables, and it
//! drops images.

use std::sync::Arc;

use pdfmd_core::{ConvertFailure, Extract, OwnedPdf, PdfConvert};

pub struct PdfCraft;

impl PdfConvert for PdfCraft {
    fn convert(&self, input: OwnedPdf) -> Result<Extract, ConvertFailure> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| convert_inner(input))) {
            Ok(result) => result,
            Err(_) => Err(ConvertFailure::new(500, "PdfCraft failed while reading this PDF")),
        }
    }
}

fn convert_inner(input: OwnedPdf) -> Result<Extract, ConvertFailure> {
    let mut session = pdfcraft_engine::Session::default();
    let password = input.password.as_deref().filter(|value| !value.is_empty());
    let id = match session.open(input.name, None, Arc::new(input.bytes), password) {
        Ok(id) => id,
        Err(err) => return Err(ConvertFailure::new(400, err.to_string())),
    };
    let Some(document) = session.get(id) else {
        return Err(ConvertFailure::new(500, "PdfCraft did not keep the document open"));
    };
    let exported = document.export_office_checked(pdfcraft_engine::compare::OfficeFormat::Markdown);
    let markdown = String::from_utf8(exported.bytes).map_err(|_| ConvertFailure::new(500, "PdfCraft markdown was not valid UTF-8"))?;
    let title = document.info.title.clone().filter(|title| !title.trim().is_empty());
    Ok(Extract { title, pages: document.info.pages.len(), truncated_pages: exported.truncated_pages, markdown })
}
