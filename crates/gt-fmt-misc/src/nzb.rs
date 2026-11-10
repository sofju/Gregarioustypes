//! NZB (Usenet download index): instead of the XML, a summary of what the NZB
//! fetches: release title, password, category, size, completeness (missing
//! segments per file), recovery blocks, posters, groups and dates, and the file
//! list grouped into archive volumes, PAR2 recovery files and the rest.

use gt_core::CancelToken;
use gt_ui::widgets::doclist::{DocList, Row, TreeBuilder};
use gt_ui::{Loaded, Probe, Provider, View, ViewEnv};
use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, Default, Clone)]
pub struct NzbFile {
    pub subject: String,
    pub name: String,
    pub poster: String,
    pub date: i64,
    pub groups: Vec<String>,
    /// (segment number, encoded bytes)
    pub segments: Vec<(u32, u64)>,
    /// Part count announced in the subject ("yEnc (1/45)").
    pub expected: Option<u32>,
}

impl NzbFile {
    pub fn bytes(&self) -> u64 {
        self.segments.iter().map(|s| s.1).sum()
    }

    /// Segments missing, judged by the announced count and gaps in the numbering.
    pub fn missing(&self) -> u32 {
        let max = self.segments.iter().map(|s| s.0).max().unwrap_or(0);
        let total = self.expected.unwrap_or(0).max(max);
        let mut nums: Vec<u32> = self.segments.iter().map(|s| s.0).collect();
        nums.sort_unstable();
        nums.dedup();
        total.saturating_sub(nums.len() as u32)
    }

    pub fn total_segments(&self) -> u32 {
        let max = self.segments.iter().map(|s| s.0).max().unwrap_or(0);
        self.expected.unwrap_or(0).max(max).max(self.segments.len() as u32)
    }
}

#[derive(Debug, Default)]
pub struct Nzb {
    pub meta: Vec<(String, String)>,
    pub files: Vec<NzbFile>,
}

/// The part count of the last "(n/m)" in a subject.
fn part_count(s: &str) -> Option<u32> {
    let i = s.rfind('(')?;
    let rest = &s[i + 1..];
    let j = rest.find(')')?;
    let (_, m) = rest[..j].split_once('/')?;
    m.trim().parse().ok()
}

/// File name from a subject: the quoted part, else the dotted word before "yEnc"/"(n/m)".
pub fn file_name(subject: &str) -> String {
    if let Some(i) = subject.find('"') {
        if let Some(j) = subject[i + 1..].find('"') {
            let n = subject[i + 1..i + 1 + j].trim();
            if !n.is_empty() {
                return n.to_string();
            }
        }
    }
    let mut s = subject.trim();
    // Leading "[01/14] - " counters.
    if s.starts_with('[') {
        if let Some(j) = s.find(']') {
            s = s[j + 1..].trim_start_matches([' ', '-']).trim();
        }
    }
    if let Some(i) = s.find(" yEnc") {
        s = &s[..i];
    }
    if s.ends_with(')') {
        if let Some(i) = s.rfind(" (") {
            s = &s[..i];
        }
    }
    let mut s = s.trim();
    // A trailing size token ("file.rar 1234567").
    if let Some((a, b)) = s.rsplit_once(' ') {
        if !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()) && a.contains('.') {
            s = a.trim();
        }
    }
    if s.is_empty() {
        subject.trim().to_string()
    } else {
        s.to_string()
    }
}

/// Parse NZB XML (UTF-8, or Windows-1252/Latin-1 bytes).
pub fn parse(data: &[u8]) -> Result<Nzb, String> {
    let text = match std::str::from_utf8(data) {
        Ok(t) => t.to_string(),
        Err(_) => gt_core::encoding::decode_1252(data),
    };
    let mut r = Reader::from_str(&text);
    r.config_mut().trim_text(false);
    let mut nzb = Nzb::default();
    let mut cur: Option<NzbFile> = None;
    let mut meta_type: Option<String> = None;
    let mut in_group = false;
    // Text of the current <meta>/<group>: entity references arrive as separate events.
    let mut buf = String::new();
    let mut saw_root = false;
    // normalized_value works with and without quick-xml's "encoding" feature
    // (calamine turns it on for the whole build).
    let attr = |e: &quick_xml::events::BytesStart, k: &str| -> Option<String> {
        e.attributes().flatten().find(|a| a.key.local_name().as_ref() == k).and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.to_string()))
    };
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                "nzb" => saw_root = true,
                "meta" => {
                    meta_type = attr(&e, "type");
                    buf.clear();
                }
                "file" => {
                    let subject = attr(&e, "subject").unwrap_or_default();
                    cur = Some(NzbFile {
                        name: file_name(&subject),
                        expected: part_count(&subject),
                        subject,
                        poster: attr(&e, "poster").unwrap_or_default(),
                        date: attr(&e, "date").and_then(|d| d.trim().parse().ok()).unwrap_or(0),
                        ..Default::default()
                    });
                }
                "group" => {
                    in_group = true;
                    buf.clear();
                }
                "segment" => {
                    let bytes = attr(&e, "bytes").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                    let number = attr(&e, "number").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                    if let Some(f) = cur.as_mut() {
                        f.segments.push((number, bytes));
                    }
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if meta_type.is_some() || in_group {
                    buf.push_str(&t.xml10_content());
                }
            }
            Ok(Event::CData(t)) => {
                if meta_type.is_some() || in_group {
                    buf.push_str(&t.xml10_content());
                }
            }
            Ok(Event::GeneralRef(g)) => {
                if meta_type.is_some() || in_group {
                    if let Ok(Some(c)) = g.resolve_char_ref() {
                        buf.push(c);
                    } else {
                        buf.push_str(match g.as_ref() {
                            "amp" => "&",
                            "lt" => "<",
                            "gt" => ">",
                            "quot" => "\"",
                            "apos" => "'",
                            _ => "",
                        });
                    }
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "meta" => {
                    if let Some(k) = meta_type.take() {
                        nzb.meta.push((k, buf.trim().to_string()));
                    }
                }
                "group" => {
                    in_group = false;
                    if let Some(f) = cur.as_mut() {
                        let g = buf.trim();
                        if !g.is_empty() {
                            f.groups.push(g.to_string());
                        }
                    }
                }
                "file" => {
                    if let Some(f) = cur.take() {
                        nzb.files.push(f);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => {
                if nzb.files.is_empty() {
                    return Err(format!("Not a valid NZB file: {e}"));
                }
                break;
            }
            _ => {}
        }
    }
    if !saw_root && nzb.files.is_empty() {
        return Err("Not an NZB file".into());
    }
    Ok(nzb)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Archive,
    Recovery,
    Info,
    Other,
}

fn kind(name: &str) -> Kind {
    let l = name.to_ascii_lowercase();
    let ext = l.rsplit('.').next().unwrap_or("");
    if ext == "par2" || ext == "par" || (ext.len() == 3 && ext.starts_with('p') && ext[1..].chars().all(|c| c.is_ascii_digit())) {
        return Kind::Recovery;
    }
    let numbered = |p: char| ext.len() == 3 && ext.starts_with(p) && ext[1..].chars().all(|c| c.is_ascii_digit());
    if matches!(ext, "rar" | "zip" | "7z" | "tar" | "gz") || numbered('r') || numbered('s') || numbered('z') || (ext.len() == 3 && ext.chars().all(|c| c.is_ascii_digit()) && l.contains(".7z.")) {
        return Kind::Archive;
    }
    if matches!(ext, "nfo" | "sfv" | "md5" | "sha1" | "txt" | "nzb" | "diz" | "jpg" | "png") {
        return Kind::Info;
    }
    Kind::Other
}

/// "vol03+04.par2" → 4 recovery blocks.
fn par2_blocks(name: &str) -> u32 {
    let l = name.to_ascii_lowercase();
    l.rfind(".vol")
        .and_then(|i| {
            let rest = &l[i + 4..];
            let plus = rest.find('+')?;
            let end = rest[plus + 1..].find(|c: char| !c.is_ascii_digit())? + plus + 1;
            rest[plus + 1..end].parse().ok()
        })
        .unwrap_or(0)
}

fn common_title(files: &[NzbFile]) -> Option<String> {
    // The longest prefix shared by the archive volumes, cut at a dot.
    let names: Vec<&str> = files.iter().filter(|f| kind(&f.name) == Kind::Archive).map(|f| f.name.as_str()).collect();
    let first = *names.first()?;
    let mut n = first.len();
    for s in &names[1..] {
        n = n.min(first.bytes().zip(s.bytes()).take_while(|(a, b)| a == b).count());
    }
    let p = first[..n].trim_end_matches(['.', '-', '_', ' ']);
    let p = p.strip_suffix(".part").or_else(|| p.strip_suffix(".r")).or_else(|| p.strip_suffix(".7z")).unwrap_or(p);
    let p = if names.len() == 1 { first.rsplit_once('.').map(|x| x.0).unwrap_or(first) } else { p };
    (!p.is_empty()).then(|| p.to_string())
}

pub struct NzbProvider;

impl Provider for NzbProvider {
    fn id(&self) -> &'static str {
        "nzb"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        let head = String::from_utf8_lossy(&p.head[..p.head.len().min(2048)]);
        let has_root = head.contains("<nzb");
        match (p.ext.as_str(), has_root) {
            ("nzb", true) => 95,
            ("nzb", false) => 60,
            (_, true) if head.contains("newzbin.com/DTD") => 80,
            _ => 0,
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        if p.size > 512 << 20 {
            return Err("NZB file too large".into());
        }
        let data = std::fs::read(&p.path).map_err(|e| e.to_string())?;
        let nzb = parse(&data)?;
        let fallback = p.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        Ok(Box::new(NzbLoaded(nzb, fallback)))
    }
}

struct NzbLoaded(Nzb, String);

impl Loaded for NzbLoaded {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        let NzbLoaded(nzb, fallback) = *self;
        let metas = |k: &str| -> Vec<String> { nzb.meta.iter().filter(|(t, _)| t.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()).filter(|v| !v.trim().is_empty()).collect() };
        let title = metas("title").into_iter().next().or_else(|| metas("name").into_iter().next()).or_else(|| common_title(&nzb.files)).unwrap_or(fallback);
        let total: u64 = nzb.files.iter().map(|f| f.bytes()).sum();
        let segs: u32 = nzb.files.iter().map(|f| f.segments.len() as u32).sum();
        let missing: u32 = nzb.files.iter().map(|f| f.missing()).sum();
        let broken = nzb.files.iter().filter(|f| f.missing() > 0).count();
        let blocks: u32 = nzb.files.iter().map(|f| par2_blocks(&f.name)).sum();

        let mut rows = vec![Row::Heading(title)];
        for pw in metas("password") {
            rows.push(Row::mono("Password", pw));
        }
        for (k, label) in [("category", "Category"), ("tag", "Tags")] {
            let v = metas(k);
            if !v.is_empty() {
                rows.push(Row::field(label, v.join(", ")));
            }
        }
        // Other meta entries, as found.
        for (k, v) in &nzb.meta {
            let kl = k.to_ascii_lowercase();
            if !["title", "name", "password", "category", "tag"].contains(&kl.as_str()) && !v.trim().is_empty() {
                let mut label = k.clone();
                if let Some(c) = label.get_mut(..1) {
                    c.make_ascii_uppercase();
                }
                rows.push(Row::field(label, v.clone()));
            }
        }
        rows.push(Row::field("Download size", format!("{} (≈ {} decoded)", gt_core::fmt::size_long(total), gt_core::fmt::size((total as f64 / 1.0273) as u64))));
        rows.push(Row::field("Files", gt_core::fmt::group(nzb.files.len() as u64)));
        rows.push(Row::field("Articles", gt_core::fmt::group(segs as u64)));
        if missing == 0 {
            rows.push(Row::field("Completeness", "Complete: every announced article is listed"));
        } else {
            rows.push(Row::field(
                "Completeness",
                format!("{} article{} missing in {} file{}", gt_core::fmt::group(missing as u64), if missing == 1 { "" } else { "s" }, broken, if broken == 1 { "" } else { "s" }),
            ));
        }
        if blocks > 0 {
            rows.push(Row::field("Recovery", format!("{} PAR2 block{}", gt_core::fmt::group(blocks as u64), if blocks == 1 { "" } else { "s" })));
        }
        let dates: Vec<i64> = nzb.files.iter().map(|f| f.date).filter(|&d| d > 0).collect();
        if let (Some(a), Some(b)) = (dates.iter().min(), dates.iter().max()) {
            let v = if b - a < 3600 { gt_core::fmt::unix_time(*a) } else { format!("{} – {}", gt_core::fmt::unix_time(*a), gt_core::fmt::unix_time(*b)) };
            rows.push(Row::field("Posted", v));
        }
        let mut posters: Vec<String> = nzb.files.iter().map(|f| f.poster.clone()).filter(|p| !p.is_empty()).collect();
        posters.sort();
        posters.dedup();
        for (i, p) in posters.iter().take(5).enumerate() {
            rows.push(Row::field(if i == 0 { if posters.len() > 1 { "Posters" } else { "Poster" } } else { "" }, p.clone()));
        }
        if posters.len() > 5 {
            rows.push(Row::field("", format!("and {} more", posters.len() - 5)));
        }
        let mut groups: Vec<String> = nzb.files.iter().flat_map(|f| f.groups.iter().cloned()).collect();
        groups.sort();
        groups.dedup();
        if !groups.is_empty() {
            rows.push(Row::Heading(format!("Newsgroups ({})", groups.len())));
            for g in &groups {
                rows.push(Row::mono("", g.clone()));
            }
        }
        rows.push(Row::Heading(format!("Files ({})", gt_core::fmt::group(nzb.files.len() as u64))));

        // Files grouped by role; archive volumes in volume order.
        let mut files = nzb.files.clone();
        files.sort_by(|a, b| (kind(&a.name), natural(&a.name)).cmp(&(kind(&b.name), natural(&b.name))));
        let mut b = TreeBuilder::new(vec!["Articles".into(), "Posted".into()]);
        let count = |k: Kind| files.iter().filter(|f| kind(&f.name) == k).count();
        let label = |k: Kind| -> String {
            let n = count(k);
            match k {
                Kind::Archive => format!("Archive volumes ({n})"),
                Kind::Recovery => format!("PAR2 recovery ({n} files{})", if blocks > 0 { format!(", {blocks} blocks") } else { String::new() }),
                Kind::Info => format!("Info files ({n})"),
                Kind::Other => format!("Other files ({n})"),
            }
        };
        let groups_used: Vec<Kind> = [Kind::Archive, Kind::Recovery, Kind::Info, Kind::Other].into_iter().filter(|&k| count(k) > 0).collect();
        let flat = groups_used.len() <= 1;
        for f in &files {
            let k = kind(&f.name);
            let total = f.total_segments();
            let miss = f.missing();
            let arts = if miss > 0 { format!("⚠ {} of {}", total - miss, total) } else { gt_core::fmt::group(total as u64) };
            let date = if f.date > 0 { gt_core::fmt::unix_time_short(f.date) } else { String::new() };
            let path = if flat { vec![f.name.clone()] } else { vec![label(k), f.name.clone()] };
            b.add_entry(&path, f.bytes(), vec![arts, date], u32::MAX, false);
        }
        if !flat {
            for &k in &groups_used {
                let sum: u32 = files.iter().filter(|f| kind(&f.name) == k).map(|f| f.total_segments()).sum();
                let miss: u32 = files.iter().filter(|f| kind(&f.name) == k).map(|f| f.missing()).sum();
                let v = if miss > 0 { format!("⚠ {} of {}", sum - miss, sum) } else { gt_core::fmt::group(sum as u64) };
                b.dir_extra(&[label(k)], vec![v, String::new()]);
            }
        }
        let tree = b.finish(1);
        let sub = format!("{} · {} file{}{}", gt_core::fmt::size(total), files.len(), if files.len() == 1 { "" } else { "s" }, if missing > 0 { " · incomplete" } else { "" });
        let mut v = DocList::new("nzb", rows, Some(tree), Some(sub));
        v.set_extra_width(110.0);
        Box::new(v)
    }
}

/// Sort key that orders "part2" before "part10".
fn natural(s: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut num = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((std::mem::take(&mut text), num.parse().unwrap_or(0)));
                num.clear();
            }
            text.push(c);
        }
    }
    out.push((text, num.parse().unwrap_or(0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects() {
        assert_eq!(file_name(r#"[01/14] - "Some.Release.part01.rar" yEnc (1/136)"#), "Some.Release.part01.rar");
        assert_eq!(file_name("holiday_photos.zip (1/12)"), "holiday_photos.zip");
        assert_eq!(file_name("[3/9] - My File.r00 yEnc (1/50) 35000000"), "My File.r00");
        assert_eq!(part_count(r#""a.rar" yEnc (1/136)"#), Some(136));
        assert_eq!(par2_blocks("x.vol03+04.par2"), 4);
        assert!(kind("a.part01.rar") == Kind::Archive && kind("a.r07") == Kind::Archive && kind("a.7z.002") == Kind::Archive);
        assert!(kind("a.vol00+01.par2") == Kind::Recovery && kind("a.nfo") == Kind::Info);
    }

    #[test]
    fn parse_with_missing_segment() {
        let x = br#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><head><meta type="title">T &amp; U</meta><meta type="password">pw</meta></head>
<file poster="p@x" date="1700000000" subject="&quot;a.rar&quot; yEnc (1/3)"><groups><group>alt.binaries.x</group></groups>
<segments><segment bytes="100" number="1">id1@x</segment><segment bytes="50" number="3">id3@x</segment></segments></file></nzb>"#;
        let n = parse(x).unwrap();
        assert_eq!(n.meta, vec![("title".into(), "T & U".into()), ("password".into(), "pw".into())]);
        assert_eq!(n.files.len(), 1);
        let f = &n.files[0];
        assert_eq!(f.name, "a.rar");
        assert_eq!(f.bytes(), 150);
        assert_eq!(f.missing(), 1);
        assert_eq!(f.groups, vec!["alt.binaries.x".to_string()]);
    }

    #[test]
    fn latin1_bytes() {
        let x = b"<?xml version=\"1.0\" encoding=\"iso-8859-1\"?><nzb><file subject=\"Caf\xe9.txt (1/1)\"><segments><segment bytes=\"5\" number=\"1\">a@b</segment></segments></file></nzb>";
        let n = parse(x).unwrap();
        assert_eq!(n.files[0].name, "Café.txt");
    }
}
