// Re-embed the web UI whenever its built files change.
fn main() {
    println!("cargo:rerun-if-changed=web/dist");
}
