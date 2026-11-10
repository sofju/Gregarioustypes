//! Text fallback: any file that looks like text, with encoding detection.

use gt_core::encoding::{decode, detect_text, Encoding, TextKind};
use gt_core::CancelToken;
use gt_ui::widgets::text::TextView;
use gt_ui::{Loaded, Probe, Provider, View, ViewEnv};
use std::io::Read;

pub struct TextProvider;

const TEXT_EXTS: &[&str] = &[
    "txt", "log", "md", "markdown", "json", "jsonc", "xml", "yaml", "yml", "toml", "ini", "cfg", "conf", "csv", "tsv", "srt", "ass", "ssa", "vtt", "c", "h", "cc",
    "cpp", "hpp", "cs", "rs", "py", "js", "mjs", "cjs", "jsx", "tsx", "ts", "java", "kt", "go", "rb", "php", "sh", "bash", "ps1", "psm1", "bat", "cmd", "sql", "css",
    "scss", "less", "lua", "pl", "r", "swift", "m", "mm", "vb", "vbs", "reg", "inf", "wxs", "wix", "dts", "dtsi", "cue", "m3u", "m3u8", "pls", "gitignore",
    "editorconfig", "dockerfile", "makefile", "cmake", "gradle", "properties", "diff", "patch", "tex", "bib", "rst", "adoc", "nfo", "plist", "manifest", "rc",
    "def", "asm", "s", "f90", "hs", "ex", "exs", "erl", "clj", "dart", "zig", "nim", "v", "sv", "vhd", "tcl", "awk", "sed", "csproj", "vcxproj", "sln", "env",
];

/// Prose wraps by default; code does not.
const PROSE_EXTS: &[&str] = &["txt", "log", "md", "markdown", "rst", "adoc", "tex", "srt", "vtt", "nfo", ""];

impl Provider for TextProvider {
    fn id(&self) -> &'static str {
        "text"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        if p.ext == "ts" && gt_core::sniff::is_mpeg_ts(&p.head) {
            return 0;
        }
        let is_text = !matches!(detect_text(&p.head, p.complete()), TextKind::Binary);
        if p.ext == "nfo" && gt_core::sniff::is_msinfo_xml(&p.head) {
            return 70;
        }
        match (p.ext_is(TEXT_EXTS), is_text) {
            (true, true) => 50,
            (false, true) => 30,
            _ => 0,
        }
    }
    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let limit = p.settings.text.max_preview_bytes;
        let mut f = std::fs::File::open(&p.path).map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(p.size.min(limit) as usize);
        f.by_ref().take(limit).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let truncated = p.size > bytes.len() as u64;
        let enc = match detect_text(&bytes, !truncated) {
            TextKind::Text(e) => e,
            TextKind::Binary => Encoding::Windows1252,
        };
        let text = decode(&bytes, enc);
        let lines = text.lines().count();
        let prose = p.ext_is(PROSE_EXTS);
        let first_line = text.lines().next().unwrap_or("");
        let highlight = crate::highlight::syntax_for(&p.path, &p.ext, first_line).map(|syn| crate::highlight::start(syn, &text));
        let rendered = match p.ext.as_str() {
            "md" | "markdown" | "mdown" | "mkd" | "mkdn" | "mdwn" => Some("markdown"),
            "htm" | "html" | "xhtml" | "xht" | "shtml" => Some("html"),
            _ => None,
        };
        Ok(Box::new(TextData {
            rendered,
            text,
            highlight,
            numbers: !prose,
            wrap: p.settings.text.wrap && prose,
            subtitle: format!("{} · {} lines", enc.label(), gt_core::fmt::group(lines as u64)),
            note: truncated.then(|| format!("Preview shows the first {} of {}.", gt_core::fmt::size(bytes.len() as u64), gt_core::fmt::size(p.size))),
        }))
    }
}

struct TextData {
    rendered: Option<&'static str>,
    text: String,
    highlight: Option<std::sync::Arc<gt_ui::widgets::text::Highlight>>,
    numbers: bool,
    wrap: bool,
    subtitle: String,
    note: Option<String>,
}

impl Loaded for TextData {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        let v = TextView::new(&self.text, self.wrap, Some(self.subtitle), self.note).with_highlight(self.highlight).with_line_numbers(self.numbers);
        Box::new(match self.rendered {
            Some(r) => v.with_rendered(r),
            None => v,
        })
    }
}
