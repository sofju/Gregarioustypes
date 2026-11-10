//! .torrent metainfo: name, hashes, magnet, every tracker (by tier), web seeds, DHT
//! nodes, and the file tree. Supports v1, v2 (BEP 52) and hybrid torrents.

use crate::bencode::{Parser, Value};
use gt_core::CancelToken;
use gt_ui::widgets::doclist::{DocList, Row, TreeBuilder};
use gt_ui::{Loaded, Probe, Provider, View, ViewEnv};
use sha1::{Digest, Sha1};
use sha2::Sha256;

pub struct TorrentProvider;

#[derive(Debug, Default, Clone)]
pub struct Torrent {
    pub name: String,
    pub trackers: Vec<Vec<String>>,
    pub web_seeds: Vec<String>,
    pub nodes: Vec<String>,
    pub comment: Option<String>,
    pub created_by: Option<String>,
    pub creation_date: Option<i64>,
    pub source: Option<String>,
    pub private: bool,
    pub piece_length: u64,
    pub pieces: u64,
    pub meta_version: i64,
    pub v1_hash: Option<String>,
    pub v2_hash: Option<String>,
    /// (path components, size); padding files excluded.
    pub files: Vec<(Vec<String>, u64)>,
    pub padding_bytes: u64,
    pub single_file: bool,
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Prefer the `<key>.utf-8` variant some clients write.
fn text(d: &Value, key: &str) -> Option<String> {
    d.get(&format!("{key}.utf-8")).or_else(|| d.get(key)).and_then(|v| v.str())
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Torrent {
    pub fn parse(data: &[u8]) -> Result<Torrent, String> {
        let root = Parser::parse(data)?;
        let info = root.get("info").ok_or("no info dictionary")?;
        let span = info.span().ok_or("info is not a dictionary")?;
        let raw_info = &data[span];
        let mut t = Torrent { name: text(info, "name").unwrap_or_default(), ..Default::default() };

        // Trackers: announce-list tiers, falling back to the single announce URL.
        if let Some(tiers) = root.get("announce-list").and_then(|v| v.list()) {
            for tier in tiers {
                let urls: Vec<String> = tier.list().unwrap_or(&[]).iter().filter_map(|u| u.str()).filter(|u| !u.is_empty()).collect();
                if !urls.is_empty() {
                    t.trackers.push(urls);
                }
            }
        }
        if let Some(a) = root.get("announce").and_then(|v| v.str()) {
            if !a.is_empty() && !t.trackers.iter().flatten().any(|u| *u == a) {
                t.trackers.insert(0, vec![a]);
            }
        }
        match root.get("url-list") {
            Some(Value::Bytes(b)) => t.web_seeds.push(String::from_utf8_lossy(b).into_owned()),
            Some(Value::List(l)) => t.web_seeds.extend(l.iter().filter_map(|u| u.str())),
            _ => {}
        }
        t.web_seeds.retain(|s| !s.is_empty());
        if let Some(nodes) = root.get("nodes").and_then(|v| v.list()) {
            for n in nodes {
                if let Some([h, p]) = n.list() {
                    t.nodes.push(format!("{}:{}", h.str().unwrap_or_default(), p.int().unwrap_or(0)));
                }
            }
        }
        t.comment = text(&root, "comment").filter(|s| !s.is_empty());
        t.created_by = text(&root, "created by").filter(|s| !s.is_empty());
        t.creation_date = root.get("creation date").and_then(|v| v.int());
        t.source = text(info, "source").filter(|s| !s.is_empty());
        t.private = info.get("private").and_then(|v| v.int()) == Some(1);
        t.piece_length = info.get("piece length").and_then(|v| v.int()).unwrap_or(0).max(0) as u64;
        t.meta_version = info.get("meta version").and_then(|v| v.int()).unwrap_or(1);
        let has_v1 = info.get("pieces").is_some();
        if let Some(p) = info.get("pieces").and_then(|v| v.bytes()) {
            t.pieces = (p.len() / 20) as u64;
        }
        if has_v1 {
            t.v1_hash = Some(hexs(&Sha1::digest(raw_info)));
        }
        if t.meta_version >= 2 {
            t.v2_hash = Some(hexs(&Sha256::digest(raw_info)));
        }

        // Files: v1 list, v1 single file, or the v2 file tree.
        if let Some(files) = info.get("files").and_then(|v| v.list()) {
            for f in files {
                let len = f.get("length").and_then(|v| v.int()).unwrap_or(0).max(0) as u64;
                let path_v = f.get("path.utf-8").or_else(|| f.get("path"));
                let comps: Vec<String> = path_v.and_then(|v| v.list()).unwrap_or(&[]).iter().filter_map(|c| c.str()).collect();
                let attr = f.get("attr").and_then(|v| v.str()).unwrap_or_default();
                let padding = attr.contains('p') || comps.first().is_some_and(|c| c == ".pad") || comps.last().is_some_and(|c| c.starts_with("_____padding_file"));
                if padding {
                    t.padding_bytes += len;
                    continue;
                }
                let mut full = vec![t.name.clone()];
                full.extend(comps);
                t.files.push((full, len));
            }
        } else if let Some(len) = info.get("length").and_then(|v| v.int()) {
            t.single_file = true;
            t.files.push((vec![t.name.clone()], len.max(0) as u64));
        } else if let Some(tree) = info.get("file tree") {
            fn walk(v: &Value, prefix: &mut Vec<String>, out: &mut Vec<(Vec<String>, u64)>, depth: usize) {
                let Some(d) = v.dict() else { return };
                if depth > 64 {
                    return;
                }
                for (k, child) in d {
                    if k.is_empty() {
                        let len = child.get("length").and_then(|v| v.int()).unwrap_or(0).max(0) as u64;
                        out.push((prefix.clone(), len));
                    } else {
                        prefix.push(String::from_utf8_lossy(k).into_owned());
                        walk(child, prefix, out, depth + 1);
                        prefix.pop();
                    }
                }
            }
            let mut prefix = vec![t.name.clone()];
            walk(tree, &mut prefix, &mut t.files, 0);
            t.single_file = t.files.len() == 1 && t.files[0].0.len() == 2 && t.files[0].0[1] == t.name;
            if t.single_file {
                t.files[0].0 = vec![t.name.clone()];
            }
        }
        if t.pieces == 0 && t.piece_length > 0 {
            let total: u64 = t.files.iter().map(|f| f.1).sum::<u64>() + t.padding_bytes;
            t.pieces = total.div_ceil(t.piece_length);
        }
        Ok(t)
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.1).sum()
    }

    pub fn magnet(&self) -> String {
        let mut m = String::from("magnet:?");
        let mut parts = Vec::new();
        if let Some(h) = &self.v1_hash {
            parts.push(format!("xt=urn:btih:{h}"));
        }
        if let Some(h) = &self.v2_hash {
            parts.push(format!("xt=urn:btmh:1220{h}"));
        }
        parts.push(format!("dn={}", url_encode(&self.name)));
        for tr in self.trackers.iter().flatten() {
            parts.push(format!("tr={}", url_encode(tr)));
        }
        for ws in &self.web_seeds {
            parts.push(format!("ws={}", url_encode(ws)));
        }
        m.push_str(&parts.join("&"));
        m
    }
}

impl Provider for TorrentProvider {
    fn id(&self) -> &'static str {
        "torrent"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        match (p.ext == "torrent", gt_core::sniff::is_torrent(&p.head)) {
            (true, true) => 95,
            (false, true) => 70,
            (true, false) => 40,
            _ => 0,
        }
    }
    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        if p.size > 256 * 1024 * 1024 {
            return Err("torrent file too large".into());
        }
        let data = std::fs::read(&p.path).map_err(|e| e.to_string())?;
        Ok(Box::new(Torrent::parse(&data)?))
    }
}

impl Loaded for Torrent {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        let t = *self;
        let mut rows = vec![Row::Heading(t.name.clone())];
        rows.push(Row::field("Total size", gt_core::fmt::size_long(t.total_size())));
        rows.push(Row::field("Files", gt_core::fmt::group(t.files.len() as u64)));
        if t.piece_length > 0 {
            rows.push(Row::field("Pieces", format!("{} × {}", gt_core::fmt::group(t.pieces), gt_core::fmt::size(t.piece_length))));
        }
        let version = match (t.v1_hash.is_some(), t.v2_hash.is_some()) {
            (true, true) => "Hybrid (v1 + v2)",
            (false, true) => "v2 (BEP 52)",
            _ => "v1",
        };
        rows.push(Row::field("Format", version));
        if let Some(h) = &t.v1_hash {
            rows.push(Row::mono("Info hash v1", h.clone()));
        }
        if let Some(h) = &t.v2_hash {
            rows.push(Row::mono("Info hash v2", h.clone()));
        }
        rows.push(Row::field("Private", if t.private { "Yes (DHT and PEX disabled)" } else { "No" }));
        if let Some(d) = t.creation_date {
            rows.push(Row::field("Created", gt_core::fmt::unix_time(d)));
        }
        if let Some(c) = &t.created_by {
            rows.push(Row::field("Created by", c.clone()));
        }
        if let Some(s) = &t.source {
            rows.push(Row::field("Source", s.clone()));
        }
        if let Some(c) = &t.comment {
            rows.push(Row::field("Comment", c.replace(['\r', '\n'], " ")));
        }
        rows.push(Row::mono("Magnet link", t.magnet()));

        let tracker_count: usize = t.trackers.iter().map(|x| x.len()).sum();
        rows.push(Row::Heading(format!("Trackers ({tracker_count})")));
        if t.trackers.is_empty() {
            rows.push(Row::Text(if t.private { "No tracker URL in this file".into() } else { "Trackerless (DHT only)".into() }));
        }
        for (i, tier) in t.trackers.iter().enumerate() {
            for (j, url) in tier.iter().enumerate() {
                let key = if t.trackers.len() > 1 { if j == 0 { format!("Tier {}", i + 1) } else { String::new() } } else { format!("Tracker {}", j + 1) };
                rows.push(Row::mono(key, url.clone()));
            }
        }
        if !t.web_seeds.is_empty() {
            rows.push(Row::Heading(format!("Web seeds ({})", t.web_seeds.len())));
            for (i, w) in t.web_seeds.iter().enumerate() {
                rows.push(Row::mono(format!("Seed {}", i + 1), w.clone()));
            }
        }
        if !t.nodes.is_empty() {
            rows.push(Row::Heading(format!("DHT nodes ({})", t.nodes.len())));
            for (i, n) in t.nodes.iter().enumerate() {
                rows.push(Row::mono(format!("Node {}", i + 1), n.clone()));
            }
        }
        rows.push(Row::Heading(format!("Files ({})", gt_core::fmt::group(t.files.len() as u64))));
        let mut b = TreeBuilder::new(Vec::new());
        for (path, size) in &t.files {
            b.add_file(path, *size, Vec::new());
        }
        let tree = b.finish(2);
        let sub = format!("{} · {} file{}", gt_core::fmt::size(t.total_size()), t.files.len(), if t.files.len() == 1 { "" } else { "s" });
        Box::new(DocList::new("torrent", rows, Some(tree), Some(sub)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bstr(s: &str) -> String {
        format!("{}:{}", s.len(), s)
    }

    #[test]
    fn multi_file_with_tiers() {
        let pieces = "x".repeat(40);
        let info = format!(
            "d5:filesld6:lengthi100e4:pathl{}eed6:lengthi50e4:pathl{}{}eed4:attr1:p6:lengthi7e4:pathl{}eee4:name{}12:piece lengthi16384e6:pieces{}e",
            bstr("a.txt"),
            bstr("sub"),
            bstr("b.bin"),
            bstr(".pad"),
            bstr("Demo"),
            bstr(&pieces)
        );
        let data = format!(
            "d8:announce{}13:announce-listll{}el{}{}ee7:comment{}4:info{}e",
            bstr("udp://t1/a"),
            bstr("udp://t1/a"),
            bstr("http://t2/a"),
            bstr("http://t3/a"),
            bstr("hi"),
            info
        );
        let t = Torrent::parse(data.as_bytes()).unwrap();
        assert_eq!(t.name, "Demo");
        assert_eq!(t.trackers, vec![vec!["udp://t1/a".to_string()], vec!["http://t2/a".to_string(), "http://t3/a".to_string()]]);
        assert_eq!(t.files.len(), 2, "padding file hidden");
        assert_eq!(t.total_size(), 150);
        assert_eq!(t.pieces, 2);
        let expect = hexs(&Sha1::digest(info.as_bytes()));
        assert_eq!(t.v1_hash.as_deref(), Some(expect.as_str()));
        assert!(t.magnet().contains("tr=udp%3A%2F%2Ft1%2Fa"));
    }

    #[test]
    fn single_file() {
        let data = format!("d4:infod6:lengthi5e4:name{}12:piece lengthi16e6:pieces20:aaaaaaaaaaaaaaaaaaaaee", bstr("one.iso"));
        let t = Torrent::parse(data.as_bytes()).unwrap();
        assert!(t.single_file);
        assert_eq!(t.files, vec![(vec!["one.iso".to_string()], 5)]);
        assert!(t.trackers.is_empty());
    }
}
