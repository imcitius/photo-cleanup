//! Moving the app data: the source is never lost, the target never
//! overwritten, and a failed move leaves the old folder chosen.
//!
//! Everything runs on temporary directories.

use pc_desktop::{
    confirm_started, copy_data, measure, prepare, preview_move_with, read_bootstrap, resolve,
    revert_to_previous, switch_to_existing, Blocker, DataLayout, RelocateError, Source,
    StartupError, StoredMode, SystemDirs, DB_FILE, PARTIAL_DB, PARTIAL_THUMBS, SPACE_MARGIN,
};
use rusqlite::Connection;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const PLENTY: u64 = 1 << 40;

fn plenty(_: &Path) -> io::Result<u64> {
    Ok(PLENTY)
}

struct Env {
    connections: std::cell::RefCell<Vec<Connection>>,
    _tmp: TempDir,
    root: PathBuf,
    dirs: SystemDirs,
}

impl Env {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let dirs = SystemDirs {
            app_local_data: root.join("local/app"),
            exe_dir: Some(root.join("program")),
            portable_supported: false,
        };
        Self {
            _tmp: tmp,
            root,
            dirs,
            connections: Default::default(),
        }
    }

    /// A first launch in the system folder, with some data in it.
    fn started(&self) -> DataLayout {
        let r = resolve(&self.dirs, None).unwrap();
        let p = prepare(&self.dirs, &r).unwrap();
        confirm_started(&self.dirs, &p).unwrap();
        self.connections.borrow_mut().push(fill(&p.layout));
        p.layout
    }

    fn launch(&self) -> Result<DataLayout, StartupError> {
        let r = resolve(&self.dirs, None)?;
        let p = prepare(&self.dirs, &r)?;
        confirm_started(&self.dirs, &p)?;
        Ok(p.layout)
    }

    fn bootstrap(&self) -> Vec<u8> {
        fs::read(self.dirs.bootstrap_path()).unwrap()
    }
}

/// Rows in the real schema, some of them left in the write-ahead log, and a
/// sharded thumbnail cache.
fn fill(layout: &DataLayout) -> Connection {
    let c = Connection::open(&layout.db).unwrap();
    c.pragma_update(None, "journal_mode", "wal").unwrap();
    // Nothing is checkpointed while this connection stays open, so the rows
    // below live only in `-wal` when the copy is made.
    c.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    c.execute_batch("CREATE TABLE IF NOT EXISTS test_marker(v TEXT)")
        .unwrap();
    for i in 0..250 {
        c.execute(
            "INSERT INTO test_marker(v) VALUES (?1)",
            [format!("Снимок {i}")],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO settings(key, value) VALUES ('marker', 'исходная')",
        [],
    )
    .unwrap();
    for (shard, name, len) in [("ab", "abcd.jpg", 5_123), ("0f", "0f12.jpg", 17_001)] {
        let d = layout.thumbs.join(shard);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(name), vec![7u8; len]).unwrap();
    }
    c
}

fn marker_rows(db: &Path) -> i64 {
    let c = Connection::open(db).unwrap();
    c.query_row("SELECT count(*) FROM test_marker", [], |r| r.get(0))
        .unwrap()
}

fn setting(db: &Path) -> String {
    let c = Connection::open(db).unwrap();
    c.query_row("SELECT value FROM settings WHERE key = 'marker'", [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => vec![],
    };
    v.sort();
    v
}

#[test]
fn a_move_copies_the_data_with_its_wal_and_the_next_launch_uses_it() {
    let env = Env::new();
    let old = env.started();
    assert!(
        fs::metadata(old.db.with_file_name("photo-cleanup.db-wal"))
            .unwrap()
            .len()
            > 0,
        "the rows must still be in the WAL for this test to mean anything"
    );
    let target = env.root.join("Большой диск/Photo Cleanup data");
    let before = measure(&old).unwrap();

    let copied = copy_data(&old, Source::System, &target, plenty).unwrap();
    assert_eq!(copied.thumbs_files, 2);
    assert_eq!(copied.thumbs_bytes, 5_123 + 17_001);
    assert_eq!(marker_rows(&copied.layout.db), 250);
    assert_eq!(setting(&copied.layout.db), "исходная");
    // Only finished data and our empty namespace reservations, never a
    // byte-for-byte copy of the source WAL or an unfinished snapshot.
    assert_eq!(
        listing(&target),
        vec![
            DB_FILE.to_string(),
            format!("{DB_FILE}-journal"),
            format!("{DB_FILE}-shm"),
            format!("{DB_FILE}-wal"),
            format!("{DB_FILE}.writer-lock"),
            "thumbs".into()
        ]
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        assert_eq!(
            fs::metadata(target.join(format!("{DB_FILE}{suffix}")))
                .unwrap()
                .len(),
            0
        );
    }

    copied.commit(&env.dirs, Source::System).unwrap();
    let b = read_bootstrap(&env.dirs.bootstrap_path()).unwrap().unwrap();
    assert_eq!(b.current.mode, StoredMode::Custom);
    assert_eq!(b.current.data_dir.as_deref(), Some(target.as_path()));
    assert_eq!(b.previous.as_ref().unwrap().mode, StoredMode::System);

    let now = env.launch().unwrap();
    assert_eq!(now.dir, target);
    let b = read_bootstrap(&env.dirs.bootstrap_path()).unwrap().unwrap();
    assert!(b.previous.is_none(), "a successful start confirms the move");

    // The source is untouched: nothing deleted, everything still readable.
    assert_eq!(measure(&old).unwrap().thumbs_files, before.thumbs_files);
    assert_eq!(marker_rows(&old.db), 250);
}

#[test]
fn the_preview_changes_nothing_and_names_the_sizes() {
    let env = Env::new();
    let old = env.started();
    let boot = env.bootstrap();
    let target = env.root.join("новая папка");
    let p = preview_move_with(&old, Source::System, &target, plenty);
    assert!(p.blockers.is_empty(), "{:?}", p.blockers);
    assert!(p.size.db_bytes > 0);
    assert_eq!(p.size.thumbs_files, 2);
    assert_eq!(p.needed, p.size.total() + SPACE_MARGIN);
    assert_eq!(p.available, Some(PLENTY));
    assert!(!target.exists(), "the preview does not create the folder");
    assert_eq!(env.bootstrap(), boot);
}

#[test]
fn a_folder_with_a_database_is_never_copied_over() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("чужая");
    fs::create_dir_all(&target).unwrap();
    let theirs = target.join(DB_FILE);
    {
        let c = Connection::open(&theirs).unwrap();
        c.execute_batch(
            "CREATE TABLE schema_version(version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES (1);
             CREATE TABLE theirs(v); INSERT INTO theirs VALUES ('не трогать');",
        )
        .unwrap();
    }
    let bytes = fs::read(&theirs).unwrap();
    let boot = env.bootstrap();

    let p = preview_move_with(&old, Source::System, &target, plenty);
    assert!(p.existing_database);
    assert!(p.blockers.contains(&Blocker::DatabaseExists));
    let err = copy_data(&old, Source::System, &target, plenty).unwrap_err();
    assert!(matches!(err, RelocateError::Blocked { .. }), "{err:?}");
    assert_eq!(fs::read(&theirs).unwrap(), bytes);
    assert_eq!(listing(&target), vec![DB_FILE.to_string()]);
    assert_eq!(env.bootstrap(), boot);

    // Switching to it is the separate, explicit action, and it copies nothing.
    switch_to_existing(&env.dirs, Source::System, &target).unwrap();
    assert_eq!(fs::read(&theirs).unwrap(), bytes);
    let b = read_bootstrap(&env.dirs.bootstrap_path()).unwrap().unwrap();
    assert_eq!(b.current.data_dir.as_deref(), Some(target.as_path()));
    assert_eq!(b.previous.unwrap().mode, StoredMode::System);
    assert_eq!(marker_rows(&old.db), 250);
}

#[test]
fn a_file_that_is_not_our_database_is_not_switched_to() {
    let env = Env::new();
    env.started();
    let target = env.root.join("not-ours");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join(DB_FILE), b"just some bytes").unwrap();
    let boot = env.bootstrap();
    assert!(switch_to_existing(&env.dirs, Source::System, &target).is_err());
    assert_eq!(env.bootstrap(), boot);
}

#[test]
fn not_enough_space_stops_the_move_before_anything_is_written() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("small disk");
    let boot = env.bootstrap();
    let tiny = |_: &Path| Ok(1024u64);
    let p = preview_move_with(&old, Source::System, &target, tiny);
    assert!(p.blockers.iter().any(|b| matches!(
        b,
        Blocker::NotEnoughSpace {
            available: 1024,
            ..
        }
    )));
    let err = copy_data(&old, Source::System, &target, tiny).unwrap_err();
    assert!(matches!(err, RelocateError::Blocked { .. }), "{err:?}");
    assert!(!target.exists());
    assert_eq!(env.bootstrap(), boot);
    assert_eq!(marker_rows(&old.db), 250);
}

// Symbolic links need no privilege on unix only.
#[cfg(unix)]
#[test]
fn a_failed_copy_removes_only_its_own_partials_and_keeps_the_source_chosen() {
    let env = Env::new();
    let old = env.started();
    // Something the copy cannot take: a link in the cache pointing outside.
    std::os::unix::fs::symlink("/etc/hosts", old.thumbs.join("ab/link.jpg")).unwrap();
    let target = env.root.join("dest");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("someone-elses.txt"), b"keep").unwrap();
    let boot = env.bootstrap();

    let err = copy_data(&old, Source::System, &target, plenty).unwrap_err();
    assert!(matches!(err, RelocateError::Copy { .. }), "{err:?}");
    assert_eq!(listing(&target), vec!["someone-elses.txt".to_string()]);
    assert_eq!(env.bootstrap(), boot);
    // And the next launch is the old folder, whole.
    let now = env.launch().unwrap();
    assert_eq!(now.dir, old.dir);
    assert_eq!(marker_rows(&now.db), 250);
}

#[test]
fn a_partial_left_by_someone_else_blocks_the_move_and_is_not_removed() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("dest");
    fs::create_dir_all(target.join(PARTIAL_THUMBS)).unwrap();
    fs::write(target.join(PARTIAL_DB), b"whose?").unwrap();
    let err = copy_data(&old, Source::System, &target, plenty).unwrap_err();
    let RelocateError::Blocked { blockers } = err else {
        panic!("{err:?}")
    };
    assert_eq!(
        blockers
            .iter()
            .filter(|b| matches!(b, Blocker::LeftoverPartial { .. }))
            .count(),
        2
    );
    assert_eq!(fs::read(target.join(PARTIAL_DB)).unwrap(), b"whose?");
    assert!(target.join(PARTIAL_THUMBS).is_dir());
}

#[test]
fn another_writer_on_the_source_stops_the_move() {
    let env = Env::new();
    let old = env.started();
    let _cli = pc_core::lock::take_writer(&old.db, "index").unwrap();
    let target = env.root.join("dest");
    let err = copy_data(&old, Source::System, &target, plenty).unwrap_err();
    assert!(matches!(err, RelocateError::Locked { .. }), "{err:?}");
    assert!(!target.exists());
}

#[test]
fn the_current_folder_and_its_cache_are_not_targets() {
    let env = Env::new();
    let old = env.started();
    let same = preview_move_with(&old, Source::System, &old.dir, plenty);
    assert!(same.blockers.contains(&Blocker::SameFolder));
    let inside = preview_move_with(&old, Source::System, &old.thumbs.join("x/y"), plenty);
    assert!(inside.blockers.contains(&Blocker::InsideCurrent));
    let relative = preview_move_with(&old, Source::System, Path::new("data"), plenty);
    assert!(relative.blockers.contains(&Blocker::RelativePath));
}

#[test]
fn portable_and_command_line_folders_are_not_moved() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("dest");
    for source in [Source::Portable, Source::Override] {
        let p = preview_move_with(&old, source, &target, plenty);
        assert!(p.blockers.contains(&Blocker::ModeFixed { source }));
        assert!(copy_data(&old, source, &target, plenty).is_err());
        assert!(switch_to_existing(&env.dirs, source, &target).is_err());
    }
    assert!(!target.exists());
}

#[test]
fn a_moved_folder_that_fails_to_start_offers_the_old_one_back() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("external/Photo Cleanup");
    let copied = copy_data(&old, Source::System, &target, plenty).unwrap();
    copied.commit(&env.dirs, Source::System).unwrap();
    // The restart finds the drive gone.
    fs::rename(&target, env.root.join("unplugged")).unwrap();
    let err = env.launch().unwrap_err();
    let StartupError::DataUnavailable { previous, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(previous.as_ref().unwrap().mode, StoredMode::System);

    let r = revert_to_previous(&env.dirs).unwrap();
    let p = prepare(&env.dirs, &r).unwrap();
    confirm_started(&env.dirs, &p).unwrap();
    assert_eq!(p.layout.dir, old.dir);
    assert_eq!(marker_rows(&p.layout.db), 250);
}

#[test]
fn a_copy_that_differs_from_the_source_is_rejected() {
    let env = Env::new();
    let old = env.started();
    let other = env.root.join("other.db");
    fs::copy(&old.db, &other).unwrap();
    let src = Connection::open(&old.db).unwrap();
    // The plain file copy misses the rows still in the WAL: exactly the
    // mistake a byte copy of the database would make.
    let err = pc_desktop::verify(&src, &other).unwrap_err();
    assert!(
        err.contains("test_marker") || err.contains("list of tables"),
        "{err}"
    );
}

#[test]
fn both_writer_locks_cover_the_gap_between_verification_and_bootstrap_commit() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("dest");
    let copied = copy_data(&old, Source::System, &target, plenty).unwrap();
    assert!(pc_core::lock::take_writer(&old.db, "late source writer").is_err());
    assert!(pc_core::lock::take_writer(&copied.layout.db, "late target writer").is_err());
    copied.commit(&env.dirs, Source::System).unwrap();
    assert!(pc_core::lock::take_writer(&old.db, "after commit").is_ok());
}

#[test]
fn switching_to_a_database_with_an_active_writer_keeps_the_old_choice() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("dest");
    let copied = copy_data(&old, Source::System, &target, plenty).unwrap();
    let boot = env.bootstrap();
    assert!(matches!(
        switch_to_existing(&env.dirs, Source::System, &target),
        Err(RelocateError::Locked { .. })
    ));
    assert_eq!(env.bootstrap(), boot);
    drop(copied);
    switch_to_existing(&env.dirs, Source::System, &target).unwrap();
}

#[test]
fn a_partial_created_after_preview_belongs_to_someone_else() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("dest");
    let probes = std::cell::Cell::new(0);
    let probe = |_: &Path| {
        probes.set(probes.get() + 1);
        if probes.get() == 2 {
            // The recheck has already checked the name; reserve it before
            // the copy itself claims ownership.
            fs::write(target.join(PARTIAL_DB), b"not ours").unwrap();
        }
        Ok(PLENTY)
    };
    assert!(copy_data(&old, Source::System, &target, probe).is_err());
    assert_eq!(fs::read(target.join(PARTIAL_DB)).unwrap(), b"not ours");
    assert_eq!(marker_rows(&old.db), 250);
}

#[test]
fn publishing_never_replaces_a_name_that_appeared_during_copy() {
    let env = Env::new();
    for directory in [false, true] {
        let src = env
            .root
            .join(if directory { "src-dir" } else { "src-file" });
        let dst = env
            .root
            .join(if directory { "dst-dir" } else { "dst-file" });
        if directory {
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dst).unwrap();
        } else {
            fs::write(&src, b"copy").unwrap();
            fs::write(&dst, b"theirs").unwrap();
        }
        assert!(pc_core::disk::rename_no_replace(&src, &dst).is_err());
        assert!(src.exists());
        if !directory {
            assert_eq!(fs::read(&dst).unwrap(), b"theirs");
        }
    }
}

#[test]
fn a_restart_spawn_failure_restores_the_bootstrap_and_preserves_both_copies() {
    let env = Env::new();
    let old = env.started();
    let boot = env.bootstrap();
    let target = env.root.join("dest");
    copy_data(&old, Source::System, &target, plenty)
        .unwrap()
        .commit(&env.dirs, Source::System)
        .unwrap();
    let error = pc_desktop::restart_or_restore(&env.dirs, || Err("injected spawn failure".into()))
        .unwrap_err();
    assert_eq!(error, "injected spawn failure");
    assert_eq!(env.bootstrap(), boot);
    assert_eq!(env.launch().unwrap().dir, old.dir);
    assert_eq!(marker_rows(&old.db), 250);
    assert_eq!(marker_rows(&target.join(DB_FILE)), 250);
}

#[test]
fn a_foreign_target_wal_survives_copy_and_restart() {
    let env = Env::new();
    let old = env.started();
    let c = Connection::open(&old.db).unwrap();
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    let other = env.root.join("other.db");
    fs::copy(&old.db, &other).unwrap();
    let foreign_db = Connection::open(&other).unwrap();
    foreign_db
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             UPDATE settings SET value='foreign' WHERE key='marker'",
        )
        .unwrap();
    let target = env.root.join("dest");
    fs::create_dir(&target).unwrap();
    let wal = target.join(format!("{DB_FILE}-wal"));
    fs::copy(env.root.join("other.db-wal"), &wal).unwrap();
    let foreign = fs::read(&wal).unwrap();
    assert!(!foreign.is_empty(), "exercise a real uncheckpointed WAL");
    let boot = env.bootstrap();
    let result = copy_data(&old, Source::System, &target, plenty);
    // Exercise the full old failure path, rather than only asserting that
    // preview should reject it: SQLite removed this foreign WAL on restart.
    let permitted = result.is_ok();
    if let Ok(copied) = result {
        copied.commit(&env.dirs, Source::System).unwrap();
        drop(pc_db::Db::open(&target.join(DB_FILE)).unwrap());
        assert_eq!(setting(&target.join(DB_FILE)), "исходная");
    }
    assert!(
        fs::read(&wal).is_ok_and(|b| b == foreign),
        "foreign WAL changed"
    );
    assert!(!permitted, "a foreign sidecar must block the move");
    assert_eq!(env.bootstrap(), boot);
    assert_eq!(marker_rows(&old.db), 250);
}

#[test]
fn a_snapshot_error_preserves_a_foreign_partial_wal() {
    let env = Env::new();
    let old = prepare(&env.dirs, &resolve(&env.dirs, None).unwrap())
        .unwrap()
        .layout;
    let boot = env.bootstrap();
    fs::write(&old.db, b"broken sqlite input").unwrap();
    let target = env.root.join("dest");
    fs::create_dir(&target).unwrap();
    let wal = target.join(format!("{PARTIAL_DB}-wal"));
    fs::write(&wal, b"not owned by this relocation").unwrap();
    assert!(copy_data(&old, Source::System, &target, plenty).is_err());
    assert_eq!(env.bootstrap(), boot);
    assert_eq!(fs::read(&old.db).unwrap(), b"broken sqlite input");
    assert_eq!(
        fs::read(wal).ok(),
        Some(b"not owned by this relocation".to_vec()),
        "snapshot cleanup deleted a foreign sidecar"
    );
}

#[test]
fn every_foreign_sqlite_sidecar_blocks_copy_without_being_changed() {
    let env = Env::new();
    let old = env.started();
    let boot = env.bootstrap();
    for name in [DB_FILE, PARTIAL_DB] {
        for suffix in ["-wal", "-shm", "-journal"] {
            let target = env.root.join(format!("{name}{suffix}"));
            fs::create_dir(&target).unwrap();
            let foreign = target.join(format!("{name}{suffix}"));
            fs::write(&foreign, b"foreign recovery data").unwrap();
            let preview = preview_move_with(&old, Source::System, &target, plenty);
            assert!(!preview.blockers.is_empty(), "{}", foreign.display());
            assert!(
                !preview.existing_database,
                "sidecars are not a DB to switch to"
            );
            assert!(copy_data(&old, Source::System, &target, plenty).is_err());
            assert_eq!(fs::read(foreign).unwrap(), b"foreign recovery data");
            assert_eq!(listing(&target), vec![format!("{name}{suffix}")]);
            assert_eq!(env.bootstrap(), boot);
        }
    }
    assert_eq!(marker_rows(&old.db), 250);
}

#[test]
fn sidecars_appearing_after_either_preview_are_preserved() {
    let env = Env::new();
    let old = env.started();
    let boot = env.bootstrap();
    for probe_number in [1, 2] {
        for name in [DB_FILE, PARTIAL_DB] {
            for suffix in ["-wal", "-shm", "-journal"] {
                let target = env.root.join(format!("{probe_number}-{name}{suffix}"));
                fs::create_dir(&target).unwrap();
                let foreign = target.join(format!("{name}{suffix}"));
                let probes = std::cell::Cell::new(0);
                let probe = |_: &Path| {
                    probes.set(probes.get() + 1);
                    if probes.get() == probe_number {
                        // Called after the preview's collision checks; the
                        // second probe runs with both writer locks held.
                        fs::write(&foreign, b"late foreign data").unwrap();
                    }
                    Ok(PLENTY)
                };
                assert!(copy_data(&old, Source::System, &target, probe).is_err());
                assert_eq!(fs::read(foreign).unwrap(), b"late foreign data");
                assert!(!target.join(DB_FILE).exists());
                assert!(!target.join(PARTIAL_DB).exists());
                for own_suffix in ["-wal", "-shm", "-journal"] {
                    let own = target.join(format!("{DB_FILE}{own_suffix}"));
                    if name != DB_FILE || suffix != own_suffix {
                        assert!(!own.exists(), "only our reservations may be cleaned");
                    }
                }
                assert_eq!(env.bootstrap(), boot);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn symlinks_and_dangling_names_block_copy_even_after_preview() {
    use std::os::unix::fs::symlink;
    let env = Env::new();
    let old = env.started();
    let boot = env.bootstrap();
    let mut names = vec![
        DB_FILE.to_string(),
        PARTIAL_DB.into(),
        PARTIAL_THUMBS.into(),
    ];
    for name in [DB_FILE, PARTIAL_DB] {
        names.extend(["-wal", "-shm", "-journal"].map(|s| format!("{name}{s}")));
    }
    for after_preview in [false, true] {
        for dangling in [false, true] {
            for name in &names {
                let target = env.root.join(format!("{after_preview}-{dangling}-{name}"));
                fs::create_dir(&target).unwrap();
                let referent = target.join("foreign");
                if !dangling {
                    fs::write(&referent, b"do not follow").unwrap();
                }
                let link = target.join(name);
                if !after_preview {
                    symlink(&referent, &link).unwrap();
                    let preview = preview_move_with(&old, Source::System, &target, plenty);
                    assert!(!preview.blockers.is_empty());
                    assert!(!preview.existing_database);
                }
                let probes = std::cell::Cell::new(0);
                let probe = |_: &Path| {
                    probes.set(probes.get() + 1);
                    if after_preview && probes.get() == 2 {
                        symlink(&referent, &link).unwrap();
                    }
                    Ok(PLENTY)
                };
                assert!(copy_data(&old, Source::System, &target, probe).is_err());
                assert_eq!(fs::read_link(link).unwrap(), referent);
                if dangling {
                    assert!(!referent.exists());
                } else {
                    assert_eq!(fs::read(referent).unwrap(), b"do not follow");
                }
                assert_eq!(env.bootstrap(), boot);
            }
        }
    }
}

#[test]
fn a_snapshot_failure_removes_its_private_namespace_and_reservations() {
    let env = Env::new();
    let old = prepare(&env.dirs, &resolve(&env.dirs, None).unwrap())
        .unwrap()
        .layout;
    fs::write(&old.db, b"broken sqlite input").unwrap();
    let boot = env.bootstrap();
    let target = env.root.join("dest");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("foreign"), b"keep").unwrap();
    assert!(matches!(
        copy_data(&old, Source::System, &target, plenty),
        Err(RelocateError::Copy { .. })
    ));
    assert_eq!(
        listing(&target),
        vec!["foreign".into(), format!("{DB_FILE}.writer-lock")]
    );
    assert_eq!(fs::read(target.join("foreign")).unwrap(), b"keep");
    assert_eq!(env.bootstrap(), boot);
    assert_eq!(fs::read(old.db).unwrap(), b"broken sqlite input");
}

#[test]
fn replaced_or_modified_final_sidecars_block_bootstrap_commit() {
    let env = Env::new();
    let old = env.started();
    let boot = env.bootstrap();
    for replace in [false, true] {
        for suffix in ["-wal", "-shm", "-journal"] {
            let target = env.root.join(format!("{replace}{suffix}"));
            let copied = copy_data(&old, Source::System, &target, plenty).unwrap();
            let foreign = target.join(format!("{DB_FILE}{suffix}"));
            if replace {
                fs::remove_file(&foreign).unwrap();
            }
            fs::write(&foreign, b"late foreign data").unwrap();
            assert!(copied.commit(&env.dirs, Source::System).is_err());
            assert_eq!(fs::read(foreign).unwrap(), b"late foreign data");
            assert_eq!(env.bootstrap(), boot);
            assert_eq!(marker_rows(&old.db), 250);
        }
    }
}

#[test]
fn owned_empty_sidecars_survive_commit_and_sqlite_can_restart_twice() {
    let env = Env::new();
    let old = env.started();
    let target = env.root.join("dest");
    copy_data(&old, Source::System, &target, plenty)
        .unwrap()
        .commit(&env.dirs, Source::System)
        .unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        assert_eq!(
            fs::read(target.join(format!("{DB_FILE}{suffix}"))).unwrap(),
            b""
        );
    }
    for _ in 0..2 {
        assert_eq!(env.launch().unwrap().dir, target);
        drop(pc_db::Db::open(&target.join(DB_FILE)).unwrap());
        assert_eq!(marker_rows(&target.join(DB_FILE)), 250);
        assert_eq!(setting(&target.join(DB_FILE)), "исходная");
        assert_eq!(marker_rows(&old.db), 250);
    }
}
