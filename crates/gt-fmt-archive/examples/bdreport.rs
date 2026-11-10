//! Blu-ray summary and full BDInfo report via bdinfo-rs: bdreport <iso or folder>
use gt_core::CancelToken;
use std::path::Path;

fn main() {
    let p = std::env::args().nth(1).expect("path");
    let c = CancelToken::new();
    let t = std::time::Instant::now();
    match gt_fmt_archive::bdinfo::summary(Path::new(&p), &c) {
        Ok(d) => {
            println!("summary in {:.1} ms: title={:?} uhd={} encrypted={} facts={:?}", t.elapsed().as_secs_f64() * 1000.0, d.title, d.uhd, d.encrypted, d.facts);
            for pl in &d.playlists {
                println!("  {} {:.2}s chapters={} presented={} clips={:?}", pl.name, pl.seconds, pl.chapters, pl.presented, pl.clips);
                for s in &pl.streams {
                    println!("    {}: {} | {} | {}", s.kind, s.codec, s.detail, s.lang);
                }
            }
        }
        Err(e) => println!("summary error: {e}"),
    }
    let t = std::time::Instant::now();
    let r = gt_fmt_archive::bdinfo::run(Path::new(&p), &c, &|s| eprintln!("  .. {s}"));
    match r {
        Ok(text) => println!("report in {:.1} ms, {} lines\n{}", t.elapsed().as_secs_f64() * 1000.0, text.lines().count(), text.lines().take(70).collect::<Vec<_>>().join("\n")),
        Err(e) => println!("report error: {e}"),
    }
}
