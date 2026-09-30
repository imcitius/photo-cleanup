//! On a system where the program cannot prove that other accounts cannot
//! redirect or re-permission the folders of a move (everything but macOS
//! and Linux, Windows included), copying is refused before anything is
//! written: no target folder, staging, reservation or lock appears, and the
//! bootstrap and the current data stay as they were.
//!
//! Only compiled there; macOS and Linux run `relocate.rs` instead.
#![cfg(not(any(target_os = "macos", target_os = "linux")))]

use pc_desktop::{
    confirm_started, copy_data, prepare, preview_move_with, resolve, Blocker, RelocateError,
    Source, SystemDirs,
};
use std::fs;
use std::io;
use std::path::Path;

fn plenty(_: &Path) -> io::Result<u64> {
    Ok(1 << 40)
}

#[test]
fn copying_is_refused_before_anything_is_written() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dirs = SystemDirs {
        app_local_data: root.join("local/app"),
        exe_dir: Some(root.join("program")),
        portable_supported: false,
    };
    let r = resolve(&dirs, None).unwrap();
    let p = prepare(&dirs, &r).unwrap();
    confirm_started(&dirs, &p).unwrap();
    let old = p.layout.clone();
    let db = fs::read(&old.db).unwrap();
    let boot = fs::read(dirs.bootstrap_path()).ok();
    let source_entries = |dir: &Path| {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        v.sort();
        v
    };
    let before = source_entries(&old.dir);
    let target = root.join("moved");

    let preview = preview_move_with(&dirs, &old, Source::System, &target, plenty);
    assert!(
        preview
            .blockers
            .iter()
            .any(|b| matches!(b, Blocker::UnprotectedFolder { .. })),
        "{:?}",
        preview.blockers
    );
    for _ in 0..2 {
        let err = copy_data(&dirs, &old, Source::System, &target, plenty).unwrap_err();
        assert!(matches!(err, RelocateError::Blocked { .. }), "{err:?}");
        assert!(!target.exists(), "nothing is created at the target");
        assert_eq!(fs::read(dirs.bootstrap_path()).ok(), boot);
        assert_eq!(source_entries(&old.dir), before);
        assert_eq!(fs::read(&old.db).unwrap(), db);
    }
}
