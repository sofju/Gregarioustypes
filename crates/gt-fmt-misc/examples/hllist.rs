fn main() {
    let ss = two_face::syntax::extra_newlines();
    for s in ss.syntaxes() {
        let n = s.name.to_lowercase();
        if ["shell", "awk", "log", "basic", "assembl", "propert", "srt", "subrip", "vtt", "reg", "batch", "ini", "nfo"].iter().any(|k| n.contains(k)) {
            println!("{} [{}]", s.name, s.file_extensions.join(" "));
        }
    }
}
