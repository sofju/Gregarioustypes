//! Generic info cards for files and folders (Quick Look's fallback).

use gt_core::CancelToken;
use gt_ui::widgets::card::{CardData, InfoCard};
use gt_ui::{Loaded, Probe, Provider, View, ViewEnv};
use std::path::Path;
use std::time::{Duration, Instant};

fn icon(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let p = path.display().to_string();
    gt_shell::info::image(&p, 256, true).or_else(|| gt_shell::info::image(&p, 256, false)).map(|b| (b.width, b.height, b.data))
}

fn times(meta: &std::fs::Metadata, facts: &mut Vec<(String, String)>) {
    if let Ok(t) = meta.modified() {
        facts.push(("Modified".into(), gt_core::fmt::system_time(t)));
    }
    if let Ok(t) = meta.created() {
        facts.push(("Created".into(), gt_core::fmt::system_time(t)));
    }
}

pub struct CardProvider;

impl Provider for CardProvider {
    fn id(&self) -> &'static str {
        "card"
    }
    fn score(&self, _p: &Probe) -> u32 {
        1
    }
    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let meta = std::fs::metadata(&p.path).map_err(|e| e.to_string())?;
        let path_s = p.path.display().to_string();
        let kind = gt_shell::info::type_name(&path_s);
        let mut facts = vec![("Size".into(), gt_core::fmt::size_long(meta.len()))];
        times(&meta, &mut facts);
        if let Some(app) = gt_shell::info::default_app(&path_s) {
            facts.push(("Opens with".into(), app));
        }
        facts.push(("Location".into(), p.path.parent().map(|d| d.display().to_string()).unwrap_or_default()));
        Ok(Box::new(Card(CardData { icon: icon(&p.path), name: p.name(), kind: if kind.is_empty() { "File".into() } else { kind }, facts, note: None })))
    }
}

pub struct FolderProvider;

impl Provider for FolderProvider {
    fn id(&self) -> &'static str {
        "folder"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            90
        } else {
            0
        }
    }
    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let meta = std::fs::metadata(&p.path).map_err(|e| e.to_string())?;
        // Walk for up to 400 ms; report a lower bound if the folder is huge.
        let deadline = Instant::now() + Duration::from_millis(400);
        let (mut files, mut dirs, mut bytes, mut complete) = (0u64, 0u64, 0u64, true);
        let mut stack = vec![p.path.clone()];
        'walk: while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                if cancel.is_cancelled() || Instant::now() > deadline {
                    complete = false;
                    break 'walk;
                }
                match e.file_type() {
                    Ok(t) if t.is_dir() => {
                        dirs += 1;
                        stack.push(e.path());
                    }
                    Ok(_) => {
                        files += 1;
                        bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                    Err(_) => {}
                }
            }
        }
        let more = if complete { "" } else { "+" };
        let mut facts = vec![
            ("Contents".into(), format!("{}{more} files, {}{more} folders", gt_core::fmt::group(files), gt_core::fmt::group(dirs))),
            ("Size".into(), format!("{}{more}", gt_core::fmt::size_long(bytes))),
        ];
        times(&meta, &mut facts);
        let mut kind = "Folder".to_string();
        if p.path.join("BDMV").join("index.bdmv").is_file() {
            kind = "Blu-ray folder (BDMV)".into();
        } else if p.path.join("VIDEO_TS").is_dir() {
            kind = "DVD-Video folder (VIDEO_TS)".into();
        }
        Ok(Box::new(Card(CardData { icon: icon(&p.path), name: p.name(), kind, facts, note: (!complete).then(|| "Large folder: totals are partial.".into()) })))
    }
}

struct Card(CardData);

impl Loaded for Card {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        Box::new(InfoCard::new(&env.gfx.dc, self.0))
    }
}
