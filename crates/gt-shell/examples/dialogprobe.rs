//! Find a file dialog by title, hook it and print its selection.
//! Usage: dialogprobe "<dialog title>"
use windows::core::HSTRING;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

fn main() {
    let title = std::env::args().nth(1).unwrap_or_default();
    let Ok(h) = (unsafe { FindWindowW(windows::core::w!("#32770"), &HSTRING::from(title.as_str())) }) else {
        println!("no dialog titled {title}");
        return;
    };
    let t = std::time::Instant::now();
    let Some(mut link) = gt_shell::dialog::Link::new(h) else {
        println!("no link");
        return;
    };
    for i in 0..2 {
        match link.query(h, 500) {
            Some((folder, items, focused)) => println!("query {i} in {:?}: folder {folder}, focused {focused}, items {items:?}", t.elapsed()),
            None => println!("query {i}: no answer after {:?}", t.elapsed()),
        }
    }
}
