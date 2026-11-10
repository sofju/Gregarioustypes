//! Documents: PDF (PDFium), Illustrator, EPS/PostScript (Ghostscript), SVG
//! (resvg), RTF (RichEdit), spreadsheets (native grid), and office documents
//! converted by a warm LibreOffice service.

pub mod ghostscript;
pub mod helper;
pub mod office;
pub mod paged;
pub mod pages;
pub mod pdf;
pub mod pdfium;
pub mod props;
pub mod raster;
pub mod provider;
pub mod rtf;
pub mod sheet;
pub mod staged;
pub mod svg;

use gt_ui::Registry;

pub fn register(r: &mut Registry) {
    r.add(provider::PdfProvider);
    r.add(provider::PostScriptProvider);
    r.add(provider::SvgProvider);
    r.add(rtf::RtfProvider);
    r.add(sheet::SheetProvider);
    r.add(provider::OfficeProvider);
}

/// Warm up the cheap engines (PDFium, the SVG font index) at viewer start.
pub fn prewarm() {
    let _ = std::thread::Builder::new().name("gt-doc-warm".into()).spawn(|| {
        pdfium::preload();
    });
    svg::prewarm();
}
