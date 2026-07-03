// Cargo does not track files pulled in by `include_dir!`/`include_bytes!`,
// so changing the embedded animation frames (or any baked-in asset) does not
// invalidate the cached binary — it would silently ship stale frames.
// Re-run the build whenever the embedded asset directories change.
fn main() {
    println!("cargo:rerun-if-changed=assets/bt-frames");
    println!("cargo:rerun-if-changed=assets");
}
