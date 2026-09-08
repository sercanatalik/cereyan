//! Ensure the UI dist folder exists so rust-embed can compile without a UI
//! build. A placeholder page is written when nothing has been built yet.

use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist = manifest.join("../../ui/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    if !dist.join("index.html").exists() {
        let _ = fs::create_dir_all(&dist);
        let _ = fs::write(
            dist.join("index.html"),
            "<!doctype html><title>cereyan</title><body style=\"font-family:system-ui;padding:2rem\">\
             <h1>cereyan</h1><p>The UI was not built into this wheel. Run <code>just ui</code> \
             and rebuild, or use the API at <a href=\"/api/openapi.json\">/api/openapi.json</a>.</p></body>",
        );
    }
}
