//! Syntax colours for text and code: bat's syntax collection (two-face over
//! syntect, Oniguruma regexes) with the One Half Dark colours, computed
//! on a background thread line by line while the text is already on screen.

use gt_ui::widgets::text::{Highlight, Span};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use syntect::highlighting::{FontStyle, HighlightIterator, HighlightState, Highlighter, Theme};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Lines coloured at most; later lines stay plain.
pub const MAX_LINES: usize = 60_000;
/// Longer lines (minified code) are left plain: regexes on them are very slow.
const MAX_LINE_CHARS: usize = 3000;
/// Lines handed to the view at a time.
const BATCH: usize = 200;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(two_face::syntax::extra_newlines)
}

fn theme() -> &'static Theme {
    static T: OnceLock<Theme> = OnceLock::new();
    T.get_or_init(|| two_face::theme::extra().get(two_face::theme::EmbeddedThemeName::OneHalfDark).clone())
}

/// Load the syntax set and theme in the background (viewer start-up).
pub fn prewarm() {
    let _ = std::thread::Builder::new().name("gt-syntax-warm".into()).spawn(|| {
        let t = std::time::Instant::now();
        let n = syntaxes().syntaxes().len();
        let _ = theme();
        tracing::info!("syntaxes: {n} languages in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    });
}

/// The syntax for a file: by file name (Makefile, Dockerfile...), extension,
/// then first line (shebang, <?xml ...>). Plain text gets none.
pub fn syntax_for(path: &Path, ext: &str, first_line: &str) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    ss.find_syntax_by_extension(&name)
        .or_else(|| ss.find_syntax_by_extension(&name.to_ascii_lowercase()))
        .or_else(|| (!ext.is_empty()).then(|| ss.find_syntax_by_extension(ext)).flatten())
        .or_else(|| ss.find_syntax_by_first_line(first_line))
        .filter(|s| s.name != "Plain Text")
}

/// Columns of the view's tab-expanded line (tab stops of 4, as `TextView`).
fn columns(line: &str) -> Vec<u32> {
    let mut v = Vec::with_capacity(line.len() + 1);
    let mut col = 0u32;
    for c in line.chars() {
        v.push(col);
        col += if c == '\t' { 4 - col % 4 } else { 1 };
    }
    v.push(col);
    v
}

fn spans_of(line: &str, parts: &[(syntect::highlighting::Style, &str)], default_fg: u32) -> Vec<Span> {
    let cols = columns(line);
    let mut out: Vec<Span> = Vec::new();
    let mut ch = 0usize;
    for (style, text) in parts {
        let n = text.chars().count();
        let (c0, c1) = (cols[ch.min(cols.len() - 1)], cols[(ch + n).min(cols.len() - 1)]);
        ch += n;
        let f = style.foreground;
        let rgb = (f.r as u32) << 16 | (f.g as u32) << 8 | f.b as u32;
        let bold = style.font_style.contains(FontStyle::BOLD);
        let italic = style.font_style.contains(FontStyle::ITALIC);
        if c1 <= c0 || (rgb == default_fg && !bold && !italic) || text.trim().is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.rgb == rgb && last.bold == bold && last.italic == italic && last.start + last.len == c0 => last.len = c1 - last.start,
            _ => out.push(Span { start: c0, len: c1 - c0, rgb, bold, italic }),
        }
    }
    out
}

/// Colour `text` for `syntax` on a new thread; the view reads the result as it
/// grows. Stops early when the view is gone.
pub fn start(syntax: &'static SyntaxReference, text: &str) -> Arc<Highlight> {
    let lines: Vec<String> = text.split('\n').take(MAX_LINES).map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
    let limit = (text.split('\n').count() > MAX_LINES).then_some(MAX_LINES);
    let h = Highlight::new(syntax.name.clone(), limit);
    let h2 = h.clone();
    let _ = std::thread::Builder::new().name("gt-highlight".into()).spawn(move || {
        let t = std::time::Instant::now();
        let ss = syntaxes();
        let th = theme();
        let default_fg = th.settings.foreground.map(|c| (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32).unwrap_or(0xD4D4D4);
        let highlighter = Highlighter::new(th);
        let mut parse = ParseState::new(syntax);
        let mut state = HighlightState::new(&highlighter, ScopeStack::new());
        let mut batch = Vec::with_capacity(BATCH);
        let mut buf = String::new();
        for line in &lines {
            // Only the highlighter holds it: the view was closed.
            if Arc::strong_count(&h2) == 1 {
                return;
            }
            if line.chars().count() > MAX_LINE_CHARS {
                batch.push(Vec::new());
            } else {
                buf.clear();
                buf.push_str(line);
                buf.push('\n');
                let Ok(ops) = parse.parse_line(&buf, ss) else { break };
                let parts: Vec<_> = HighlightIterator::new(&mut state, &ops, &buf, &highlighter).collect();
                batch.push(spans_of(line, &parts, default_fg));
            }
            if batch.len() >= BATCH {
                h2.extend(batch.drain(..));
            }
        }
        h2.extend(batch);
        h2.finish();
        tracing::debug!("{}: {} lines coloured in {:.0} ms", h2.language, lines.len(), t.elapsed().as_secs_f64() * 1000.0);
    });
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_languages() {
        let s = |p: &str, first: &str| {
            let path = Path::new(p);
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            syntax_for(path, &ext, first).map(|s| s.name.clone())
        };
        assert_eq!(s("main.rs", "").as_deref(), Some("Rust"));
        assert_eq!(s("app.ts", "").as_deref(), Some("TypeScript"));
        assert_eq!(s("Cargo.toml", "").as_deref(), Some("TOML"));
        assert_eq!(s("Dockerfile", "").as_deref(), Some("Dockerfile"));
        assert_eq!(s("build.ps1", "").as_deref(), Some("PowerShell"));
        assert_eq!(s("script", "#!/usr/bin/env python3").as_deref(), Some("Python"));
        assert!(s("notes.txt", "hello").is_none());
    }

    #[test]
    fn colours_with_tabs() {
        let syn = syntax_for(Path::new("a.rs"), "rs", "").unwrap();
        let h = start(syn, "fn main() {\n\tlet x = \"hi\";\n}\n");
        while !h.is_done() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let lines = h.snapshot();
        // `fn` is a keyword: coloured from column 0.
        assert!(lines[0].iter().any(|s| s.start == 0 && s.len == 2));
        // The tab expands to 4 columns: `let` starts at column 4.
        assert!(lines[1].iter().any(|s| s.start == 4 && s.len == 3));
    }
}
