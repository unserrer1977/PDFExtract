//! PDF to Markdown HTTP service, with PdfCraft supplied by the server binary.

mod http;
mod logic;
mod openapi;
mod ssrf;

pub use http::{boot, keys_match, router, AppState, PdfConvert};
pub use logic::{compose, looks_like_pdf, ConvertFailure, Conversion, Extract, OwnedPdf, IMAGE_NOTICE, MAX_PDF_BYTES};
pub use ssrf::screen_url;
