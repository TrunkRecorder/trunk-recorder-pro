// Rebuild when the web interface is rebuilt: its files are embedded (rust-embed).
fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");
}
