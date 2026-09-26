//! Tempfile-only bridge for Playwright's data-folder regression tests.
//! No native window or restart is involved; stdin drives the real relocation API.
use pc_desktop::*;
use rusqlite::Connection;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

fn enough(_: &Path) -> io::Result<u64> {
    Ok(1 << 40)
}

fn marker(path: &Path) -> String {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT value FROM ui_marker", [], |r| r.get(0))
        .unwrap()
}

// Do not follow links while checking that refused targets are untouched.
fn contents(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let meta = fs::symlink_metadata(&path).unwrap();
        let bytes = if meta.is_symlink() {
            fs::read_link(&path)
                .unwrap()
                .to_string_lossy()
                .as_bytes()
                .to_vec()
        } else if meta.is_dir() {
            result.extend(contents(&path));
            Vec::new()
        } else {
            fs::read(&path).unwrap()
        };
        result.insert(path, bytes);
    }
    result
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) {
    std::os::unix::fs::symlink(from, to).unwrap();
}
#[cfg(windows)]
fn link(from: &Path, to: &Path) {
    std::os::windows::fs::symlink_file(from, to).unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let scenario = args.get(1).expect("scenario").as_str();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let dirs = SystemDirs {
        app_local_data: root.join("bootstrap"),
        exe_dir: None,
        portable_supported: false,
    };
    let source = prepare(&dirs, &resolve(&dirs, None).unwrap())
        .unwrap()
        .layout;
    Connection::open(&source.db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE ui_marker(value TEXT); INSERT INTO ui_marker VALUES ('source preserved')",
        )
        .unwrap();
    let bootstrap = fs::read(dirs.bootstrap_path()).unwrap();
    let target = root.join("target data");
    fs::create_dir(&target).unwrap();
    let mut wal_connection = None;
    match scenario {
        "recovery" => {
            copy_data(&source, Source::System, &target, enough)
                .unwrap()
                .commit(&dirs, Source::System)
                .unwrap();
            restart_or_restore(&dirs, || Err("injected spawn failure".into())).unwrap_err();
            assert_eq!(fs::read(dirs.bootstrap_path()).unwrap(), bootstrap);
            for suffix in ["-wal", "-shm", "-journal"] {
                assert_eq!(
                    fs::metadata(target.join(format!("{DB_FILE}{suffix}")))
                        .unwrap()
                        .len(),
                    0
                );
            }
        }
        "wal" => {
            fs::copy(&source.db, target.join(DB_FILE)).unwrap();
            let conn = Connection::open(target.join(DB_FILE)).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE ui_marker SET value='target WAL preserved'").unwrap();
            assert!(
                fs::metadata(target.join(format!("{DB_FILE}-wal")))
                    .unwrap()
                    .len()
                    > 0
            );
            wal_connection = Some(conn);
        }
        "sidecar" | "sidecar-link" | "sidecar-dangling" => {
            let suffix = args.get(2).expect("sidecar suffix");
            assert!(["-wal", "-shm", "-journal"].contains(&suffix.as_str()));
            let path = target.join(format!("{DB_FILE}{suffix}"));
            if scenario == "sidecar" {
                fs::write(path, b"foreign sidecar: keep these bytes").unwrap();
            } else {
                let foreign = root.join("foreign");
                if scenario == "sidecar-link" {
                    fs::write(&foreign, b"foreign referent: keep these bytes").unwrap();
                }
                link(&foreign, &path);
            }
        }
        "database-link" => link(&source.db, &target.join(DB_FILE)),
        "database-dangling" => link(&root.join("missing"), &target.join(DB_FILE)),
        "partial" => {
            fs::copy(&source.db, target.join(DB_FILE)).unwrap();
            fs::write(target.join(PARTIAL_DB), b"foreign partial").unwrap();
            fs::write(target.join(format!("{DB_FILE}-wal")), b"").unwrap();
        }
        _ => panic!("unknown scenario"),
    }
    let preview = preview_move_with(&source, Source::System, &target, enough);
    let positive = matches!(scenario, "recovery" | "wal");
    assert_eq!(preview.existing_database, positive || scenario == "partial");
    let before = contents(&root);
    println!("{}", serde_json::to_string(&preview).unwrap());
    io::stdout().flush().unwrap();
    for line in io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "switch" => {
                assert!(
                    positive,
                    "a blocked target must never reach change_data_dir"
                );
                switch_to_existing(&dirs, Source::System, &target).unwrap();
                let resolved = resolve(&dirs, None).unwrap();
                assert_eq!(resolved.layout.dir, target);
                assert_eq!(marker(&source.db), "source preserved");
                let expected = if scenario == "wal" {
                    "target WAL preserved"
                } else {
                    "source preserved"
                };
                assert_eq!(marker(&resolved.layout.db), expected);
                println!("{}", json!({"switched": true, "marker": expected}));
            }
            "check" => {
                assert!(!positive);
                assert!(copy_data(&source, Source::System, &target, enough).is_err());
                if scenario.starts_with("sidecar") {
                    assert!(switch_to_existing(&dirs, Source::System, &target).is_err());
                }
                assert_eq!(contents(&root), before);
                assert_eq!(fs::read(dirs.bootstrap_path()).unwrap(), bootstrap);
                println!("{}", json!({"unchanged": true}));
            }
            _ => panic!("unknown command"),
        }
        io::stdout().flush().unwrap();
    }
    drop(wal_connection);
    // TempDir removes only this fixture's own data on EOF, including failed UI tests.
}
