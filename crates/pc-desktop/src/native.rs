//! The window's own commands: which exist, which page may call which, and
//! what they answer. Tauri-free, so the rules are tested on every OS.
//!
//! Every business operation goes through the HTTP API, with its preview
//! token and writer lock, exactly as in a browser. These commands do only
//! what a web page cannot: a native folder dialog, showing a folder in
//! Finder/Explorer, and changing the data folder — which needs the server
//! stopped, so the server cannot do it itself.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::relocate::{measure, DataSize};
use crate::resolve::Source;
use crate::{DataLayout, SystemDirs, DB_FILE};

/// Commands the shared interface may call — from this launch's server
/// origin only (see [`interface_origin`]).
pub const INTERFACE_COMMANDS: &[&str] = &[
    "desktop_info",
    "pick_folder",
    "reveal_data_dir",
    "preview_data_dir_change",
    "change_data_dir",
];

/// Commands the bundled start-up error page may call: go back to the
/// previous data folder, or try again.
pub const ERROR_PAGE_COMMANDS: &[&str] = &["revert_data_dir", "restart_app"];

/// The one remote origin granted [`INTERFACE_COMMANDS`]: this launch's
/// server, exact address and port — no wildcard, no `localhost`. Any other
/// local server is somebody else's page.
pub fn interface_origin(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

/// The permission Tauri generates for an app command.
pub fn permission(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

/// How a data-folder change is carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeAction {
    /// Copy the current database and thumbnails there, then switch.
    Copy,
    /// Switch to the database already there; copy nothing.
    UseExisting,
}

/// What the settings screen shows about the data folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DesktopInfo {
    pub source: Source,
    /// Whether the folder can be changed from the window: not in portable
    /// mode, not when given with `--data-dir`.
    pub can_change: bool,
    pub layout: DataLayout,
    /// `None` when it could not be measured; the screen says so.
    pub size: Option<DataSize>,
    pub available: Option<u64>,
    pub system_dir: PathBuf,
    /// A database left beside the program by the batch-file distribution,
    /// offered as "switch to it" — never picked up silently.
    pub legacy_dir: Option<PathBuf>,
}

/// Gather [`DesktopInfo`]. Reads the disk (the cache is walked for its size).
pub fn desktop_info(dirs: &SystemDirs, source: Source, layout: &DataLayout) -> DesktopInfo {
    let can_change = matches!(source, Source::System | Source::Custom);
    let legacy_dir = dirs
        .exe_dir
        .as_ref()
        .filter(|d| can_change && d.join(DB_FILE).is_file() && **d != layout.dir)
        .cloned();
    DesktopInfo {
        source,
        can_change,
        layout: layout.clone(),
        size: measure(layout).ok(),
        available: pc_core::disk::available_space(&layout.dir).ok(),
        system_dir: dirs.system_data_dir(),
        legacy_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interface_origin_is_the_exact_socket() {
        let a: SocketAddr = "127.0.0.1:51234".parse().unwrap();
        assert_eq!(interface_origin(a), "http://127.0.0.1:51234");
    }

    #[test]
    fn permissions_are_named_as_tauri_generates_them() {
        assert_eq!(permission("desktop_info"), "allow-desktop-info");
        assert_eq!(
            permission("preview_data_dir_change"),
            "allow-preview-data-dir-change"
        );
    }

    #[test]
    fn the_error_page_gets_none_of_the_interface_commands() {
        for c in ERROR_PAGE_COMMANDS {
            assert!(!INTERFACE_COMMANDS.contains(c), "{c}");
        }
    }

    /// The build script declares the app's commands to Tauri, which then
    /// refuses any command it was not told about. It cannot import this
    /// crate, so it keeps its own list; this keeps the two the same.
    #[test]
    fn the_build_script_declares_exactly_these_commands() {
        let build = include_str!("../build.rs");
        let start = build.find("const COMMANDS").expect("COMMANDS in build.rs");
        let list = &build[start..build[start..].find("];").unwrap() + start];
        let mut declared: Vec<&str> = list
            .split('"')
            .enumerate()
            .filter(|(i, _)| i % 2 == 1)
            .map(|(_, s)| s)
            .collect();
        let mut ours: Vec<&str> = INTERFACE_COMMANDS
            .iter()
            .chain(ERROR_PAGE_COMMANDS)
            .copied()
            .collect();
        declared.sort_unstable();
        ours.sort_unstable();
        assert_eq!(declared, ours);
    }

    #[test]
    fn a_batch_file_database_beside_the_program_is_offered_not_used() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("Photo Cleanup");
        std::fs::create_dir_all(&exe).unwrap();
        let dirs = SystemDirs {
            app_local_data: tmp.path().join("local"),
            exe_dir: Some(exe.clone()),
            portable_supported: false,
        };
        let layout = DataLayout::in_dir(&dirs.system_data_dir());
        assert_eq!(
            desktop_info(&dirs, Source::System, &layout).legacy_dir,
            None
        );
        std::fs::write(exe.join(DB_FILE), b"").unwrap();
        let info = desktop_info(&dirs, Source::System, &layout);
        assert_eq!(info.legacy_dir, Some(exe.clone()));
        assert!(info.can_change);
        // Already the data folder (portable): nothing to offer.
        let here = DataLayout::in_dir(&exe);
        let info = desktop_info(&dirs, Source::Portable, &here);
        assert_eq!(info.legacy_dir, None);
        assert!(!info.can_change);
    }
}
