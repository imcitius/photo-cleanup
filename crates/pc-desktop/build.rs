// The build script runs on the host, so this `cfg` is the host's. That is
// the same platform as the target because desktop builds are native only (as
// the Dockerfile is): a cross build would have to change this.
fn main() {
    #[cfg(any(windows, target_os = "macos"))]
    tauri_build::build();
}
