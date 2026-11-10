//! Markdown → HTML (pulldown-cmark, GitHub flavour: tables, task lists,
//! strikethrough, footnotes, alerts), styled like GitHub's dark theme.
//! Relative links and images resolve against the file's folder.

use pulldown_cmark::{html, Options, Parser};
use std::path::Path;

pub const EXTS: &[&str] = &["md", "markdown", "mdown", "mkd", "mkdn", "mdwn", "mdtxt", "mdtext"];

const CSS: &str = r#"
:root { color-scheme: dark; }
html { background: #1c1c1d; }
body { margin: 0 auto; max-width: 880px; padding: 28px 40px 60px; color: #e6edf3; background: #1c1c1d;
  font: 16px/1.6 "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif; word-wrap: break-word; }
h1, h2, h3, h4, h5, h6 { margin: 1.4em 0 0.6em; font-weight: 600; line-height: 1.25; }
h1 { font-size: 2em; padding-bottom: .3em; border-bottom: 1px solid #3d444d; }
h2 { font-size: 1.5em; padding-bottom: .3em; border-bottom: 1px solid #3d444d; }
h3 { font-size: 1.25em; } h4 { font-size: 1em; } h6 { color: #9198a1; }
body > :first-child { margin-top: 0; }
a { color: #4cc2ff; text-decoration: none; } a:hover { text-decoration: underline; }
p, ul, ol, blockquote, table, pre, details { margin: 0 0 16px; }
code, kbd, pre { font-family: "Cascadia Mono", Consolas, monospace; font-size: 85%; }
code { background: rgba(110,118,129,.4); padding: .2em .4em; border-radius: 6px; }
pre { background: #151b23; padding: 16px; overflow: auto; border-radius: 6px; line-height: 1.45; }
pre code { background: none; padding: 0; font-size: 100%; }
kbd { border: 1px solid #3d444d; border-bottom-width: 2px; border-radius: 6px; padding: 2px 6px; background: #151b23; }
blockquote { margin-left: 0; padding: 0 1em; color: #9198a1; border-left: .25em solid #3d444d; }
table { border-collapse: collapse; display: block; width: max-content; max-width: 100%; overflow: auto; }
th, td { border: 1px solid #3d444d; padding: 6px 13px; } th { font-weight: 600; }
tr:nth-child(2n) { background: #151b23; }
img { max-width: 100%; box-sizing: border-box; }
hr { height: 4px; border: 0; background: #3d444d; margin: 24px 0; }
ul.contains-task-list, li:has(> input[type=checkbox]) { list-style: none; }
input[type=checkbox] { margin: 0 .4em 0 -1.2em; vertical-align: middle; }
.footnote-definition { font-size: 85%; color: #9198a1; display: flex; gap: .5em; }
.footnote-definition p { margin: 0; }
.markdown-alert { padding: .5em 1em; margin-bottom: 16px; border-left: .25em solid #3d444d; color: inherit; }
.markdown-alert-note { border-color: #1f6feb; } .markdown-alert-tip { border-color: #238636; }
.markdown-alert-important { border-color: #8957e5; } .markdown-alert-warning { border-color: #9e6a03; }
.markdown-alert-caution { border-color: #da3633; }
.markdown-alert-title { font-weight: 600; margin-bottom: 4px; }
"#;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The HTML body of `md`.
pub fn body(md: &str) -> String {
    let mut opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_GFM;
    opts.remove(Options::ENABLE_SMART_PUNCTUATION);
    // GitHub alerts ("> [!NOTE]") come out as blockquotes with a class.
    let parser = Parser::new_ext(md, opts);
    let mut out = String::with_capacity(md.len() * 2);
    html::push_html(&mut out, parser);
    for (kind, title) in [("note", "Note"), ("tip", "Tip"), ("important", "Important"), ("warning", "Warning"), ("caution", "Caution")] {
        out = out.replace(&format!("<blockquote class=\"markdown-alert-{kind}\">"), &format!("<blockquote class=\"markdown-alert markdown-alert-{kind}\"><p class=\"markdown-alert-title\">{title}</p>"));
    }
    out
}

pub fn page(md: &str, title: &str, base: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><base href=\"{}\"><title>{}</title><style>{CSS}</style></head><body>{}</body></html>",
        escape(base),
        escape(title),
        body(md)
    )
}

fn decode(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => gt_core::encoding::decode_1252(b),
    }
}

pub fn read(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(decode(&data))
}

/// The full page for a Markdown file (folder as base URL).
pub fn render_file(path: &Path) -> Result<String, String> {
    let md = read(path)?;
    let dir = path.parent().unwrap_or(Path::new(""));
    let mut base = crate::engine::url_for(dir);
    if !base.ends_with('/') {
        base.push('/');
    }
    let title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(page(&md, &title, &base))
}

/// Facts for the Info panel: headings, words, links, images.
pub fn facts(md: &str) -> Vec<(String, String)> {
    let mut headings = 0;
    let mut code = false;
    for l in md.lines() {
        let t = l.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            code = !code;
        } else if !code && t.starts_with('#') {
            headings += 1;
        }
    }
    let words = md.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).count();
    let images = md.matches("![").count();
    let mut f = vec![("Words".to_string(), gt_core::fmt::group(words as u64)), ("Headings".to_string(), headings.to_string())];
    if images > 0 {
        f.push(("Images".into(), images.to_string()));
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gfm_features() {
        let h = body("# T\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n\n~~gone~~\n\n> [!NOTE]\n> hi\n");
        assert!(h.contains("<table>"), "{h}");
        assert!(h.contains("type=\"checkbox\""), "{h}");
        assert!(h.contains("<del>gone</del>"), "{h}");
        assert!(h.contains("markdown-alert-note") && h.contains("markdown-alert-title"), "{h}");
    }
}
