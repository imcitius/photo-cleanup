// The build script runs on the host, so this `cfg` is the host's. That is
// the same platform as the target because desktop builds are native only (as
// the Dockerfile is): a cross build would have to change this.
//
// The app's own commands are declared here so Tauri generates an
// `allow-<command>` permission for each and refuses every command no
// capability grants — including to our own pages. The capabilities are added
// at run time (`shell.rs`), when the server's port is known. Kept equal to
// `INTERFACE_COMMANDS` + `ERROR_PAGE_COMMANDS` by a test in `src/native.rs`.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
const COMMANDS: &[&str] = &[
    "desktop_info",
    "pick_folder",
    "reveal_data_dir",
    "preview_data_dir_change",
    "change_data_dir",
    "revert_data_dir",
    "restart_app",
];

fn main() {
    #[cfg(any(windows, target_os = "macos"))]
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri-build");
}
