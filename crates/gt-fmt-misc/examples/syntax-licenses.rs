//! Prints the licences of the bundled syntax definitions and themes (bat's
//! collection, through two-face) as Markdown; scripts/collect-licenses.py
//! ships the result with the installer.

fn main() {
    print!("{}", two_face::acknowledgement::listing().to_md());
}
