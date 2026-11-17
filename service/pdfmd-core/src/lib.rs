//! PDF to Markdown HTTP service, with PdfCraft supplied by the server binary.

mod http;
mod logic;
mod openapi;
mod ssrf;
mod vision;

pub use http::{boot, keys_match, router, AppState, PdfConvert};
pub use logic::{compose, looks_like_pdf, ConvertFailure, Conversion, Extract, OwnedPdf, IMAGE_NOTICE, MAX_PDF_BYTES, VISION_NOTICE};
pub use ssrf::screen_url;
pub use vision::{ScriptedVision, VisionOcr, VisionOutcome};
