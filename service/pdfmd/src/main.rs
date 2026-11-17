mod craft;

fn main() {
    if let Err(err) = pdfmd_core::boot(craft::PdfCraft) {
        eprintln!("pdfextract: {err}");
        std::process::exit(1);
    }
}
