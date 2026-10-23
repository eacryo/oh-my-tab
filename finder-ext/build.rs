//! No special link arguments: the appex is a normal Rust executable whose main() registers the
//! principal class and hands control to Foundation's NSExtensionMain.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}
