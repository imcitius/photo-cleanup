//! The data directory is found again, and never replaced by an empty one.
//!
//! Every test runs on temporary directories standing in for the per-user
//! app directory and the program directory.

use pc_desktop::{
    choose_data_dir, confirm_started, prepare, read_bootstrap, resolve, revert_to_previous,
    Creation, NewDir, Source, StartupError, StoredMode, SystemDirs, Unavailable, BOOTSTRAP_FILE,
    DB_FILE, PORTABLE_MARKER,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Env {
    _tmp: TempDir,
    root: PathBuf,
    dirs: SystemDirs,
}

impl Env {
    /// `local` and `exe` are path fragments under one temporary root.
    fn with(local: &str, exe: &str, portable_supported: bool) -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let exe_dir = root.join(exe);
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(exe_dir.join("photo-cleanup-desktop.exe"), b"MZ").unwrap();
        let dirs = SystemDirs {
            app_local_data: root.join(local),
            exe_dir: Some(exe_dir),
            portable_supported,
        };
        Self {
            _tmp: tmp,
            root,
            dirs,
        }
    }

    fn new() -> Self {
        Self::with("local/app", "program", false)
    }

    fn exe_dir(&self) -> &Path {
        self.dirs.exe_dir.as_deref().unwrap()
    }

    fn bootstrap_bytes(&self) -> Option<Vec<u8>> {
        fs::read(self.dirs.bootstrap_path()).ok()
    }

    /// One whole launch: resolve, prepare, confirm. Returns the database.
    fn launch(&self) -> Result<PathBuf, StartupError> {
        let r = resolve(&self.dirs, None)?;
        let p = prepare(&self.dirs, &r)?;
        confirm_started(&self.dirs, &p)?;
        Ok(p.layout.db)
    }
}

fn put_marker(db: &Path, value: &str) {
    let c = rusqlite::Connection::open(db).unwrap();
    c.execute_batch("CREATE TABLE IF NOT EXISTS test_marker(v TEXT)")
        .unwrap();
    c.execute("INSERT INTO test_marker(v) VALUES (?1)", [value])
        .unwrap();
}

fn marker(db: &Path) -> String {
    let c = rusqlite::Connection::open(db).unwrap();
    c.query_row("SELECT v FROM test_marker", [], |r| r.get(0))
        .unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn unavailable(e: StartupError) -> (Source, PathBuf, Unavailable) {
    match e {
        StartupError::DataUnavailable {
            source, dir, why, ..
        } => (source, dir, why),
        other => panic!("expected DataUnavailable, got {other:?}"),
    }
}

/// Make `dir` read-only. Returns false where that does not stop writes (as
/// root, say), and the caller skips — a test that cannot fail proves nothing.
#[cfg(unix)]
fn make_read_only(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
    let probe = dir.join(".probe");
    if fs::write(&probe, b"").is_ok() {
        let _ = fs::remove_file(&probe);
        eprintln!("skipped: {} stays writable here", dir.display());
        return false;
    }
    true
}

#[cfg(unix)]
fn make_writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn the_second_launch_finds_the_archive_the_first_one_created() {
    let env = Env::new();
    let r = resolve(&env.dirs, None).unwrap();
    assert_eq!(r.source, Source::System);
    assert_eq!(r.creation, Creation::MayCreate);
    assert!(env.bootstrap_bytes().is_none(), "resolve wrote something");

    let p = prepare(&env.dirs, &r).unwrap();
    assert!(p.created);
    assert_eq!(p.layout.db, env.dirs.system_data_dir().join(DB_FILE));
    assert_eq!(p.layout.thumbs, env.dirs.system_data_dir().join("thumbs"));
    confirm_started(&env.dirs, &p).unwrap();
    put_marker(&p.layout.db, "first");

    let b = read_bootstrap(&env.dirs.bootstrap_path()).unwrap().unwrap();
    assert_eq!(b.current.mode, StoredMode::System);

    let r2 = resolve(&env.dirs, None).unwrap();
    assert_eq!(r2.creation, Creation::MustExist);
    let p2 = prepare(&env.dirs, &r2).unwrap();
    assert!(!p2.created);
    assert_eq!(p2.layout, p.layout);
    assert_eq!(marker(&p2.layout.db), "first");
}

#[test]
fn a_launch_never_writes_into_the_program_directory() {
    let env = Env::new();
    let before = listing(env.exe_dir());
    env.launch().unwrap();
    env.launch().unwrap();
    assert_eq!(listing(env.exe_dir()), before);
}

#[cfg(unix)]
#[test]
fn a_read_only_program_directory_still_launches() {
    let env = Env::new();
    if !make_read_only(env.exe_dir()) {
        return;
    }
    let result = env.launch();
    make_writable(env.exe_dir());
    let db = result.unwrap();
    assert!(db.starts_with(&env.dirs.app_local_data));
}

#[test]
fn the_portable_marker_keeps_the_data_beside_the_program_and_out_of_the_profile() {
    let env = Env::with("local/app", "program", true);
    fs::write(env.exe_dir().join(PORTABLE_MARKER), b"").unwrap();
    let r = resolve(&env.dirs, None).unwrap();
    assert_eq!(r.source, Source::Portable);
    let p = prepare(&env.dirs, &r).unwrap();
    confirm_started(&env.dirs, &p).unwrap();
    assert_eq!(p.layout.db, env.exe_dir().join("data").join(DB_FILE));
    assert!(
        !env.dirs.app_local_data.exists(),
        "portable wrote to the profile"
    );

    put_marker(&p.layout.db, "portable");
    assert_eq!(marker(&env.launch().unwrap()), "portable");
}

#[test]
fn the_portable_marker_means_nothing_where_portable_is_unsupported() {
    // macOS: a downloaded .app runs translocated from a read-only place.
    let env = Env::with("local/app", "program", false);
    fs::write(env.exe_dir().join(PORTABLE_MARKER), b"").unwrap();
    assert_eq!(resolve(&env.dirs, None).unwrap().source, Source::System);
}

#[cfg(unix)]
#[test]
fn an_unwritable_portable_directory_fails_instead_of_moving_to_the_profile() {
    let env = Env::with("local/app", "program", true);
    fs::write(env.exe_dir().join(PORTABLE_MARKER), b"").unwrap();
    if !make_read_only(env.exe_dir()) {
        return;
    }
    let result = env.launch();
    make_writable(env.exe_dir());
    let (source, _, why) = unavailable(result.unwrap_err());
    assert_eq!(source, Source::Portable);
    assert!(matches!(why, Unavailable::NotWritable(_)), "{why:?}");
    assert!(
        !env.dirs.app_local_data.exists(),
        "fell back to the profile"
    );
}

/// Set up a custom archive at `dir` and confirm it, as a user would.
fn adopt_new_custom(env: &Env, dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let r = choose_data_dir(&env.dirs, dir, NewDir::CreateNew).unwrap();
    let p = prepare(&env.dirs, &r).unwrap();
    confirm_started(&env.dirs, &p).unwrap();
    put_marker(&p.layout.db, "custom");
    p.layout.db
}

#[test]
fn an_unplugged_custom_drive_is_an_error_and_not_a_new_empty_archive() {
    let env = Env::new();
    let dir = env.root.join("external/pc");
    adopt_new_custom(&env, &dir);
    let bootstrap = env.bootstrap_bytes().unwrap();

    // The drive goes away.
    fs::rename(env.root.join("external"), env.root.join("unplugged")).unwrap();

    let (source, at, why) = unavailable(env.launch().unwrap_err());
    assert_eq!(source, Source::Custom);
    assert_eq!(at, dir);
    assert_eq!(why, Unavailable::Missing);
    assert!(!dir.exists(), "the missing folder was recreated");
    assert!(!env.dirs.system_data_dir().exists(), "fell back to system");
    assert_eq!(env.bootstrap_bytes().unwrap(), bootstrap);

    // Plugged back in, everything is where it was.
    fs::rename(env.root.join("unplugged"), env.root.join("external")).unwrap();
    assert_eq!(marker(&env.launch().unwrap()), "custom");
}

#[test]
fn a_custom_folder_that_lost_its_database_is_an_error() {
    let env = Env::new();
    let dir = env.root.join("external/pc");
    let db = adopt_new_custom(&env, &dir);
    fs::rename(&db, dir.join("moved-away.db")).unwrap();
    let (_, _, why) = unavailable(env.launch().unwrap_err());
    assert_eq!(why, Unavailable::NoDatabase);
    assert!(!db.exists(), "an empty database took its place");
}

#[test]
fn a_system_archive_that_disappeared_is_an_error_too() {
    let env = Env::new();
    let db = env.launch().unwrap();
    fs::remove_file(&db).unwrap();
    let (source, _, why) = unavailable(env.launch().unwrap_err());
    assert_eq!(source, Source::System);
    assert_eq!(why, Unavailable::NoDatabase);
    assert!(!db.exists());
}

#[cfg(unix)]
#[test]
fn a_read_only_custom_folder_is_reported_not_opened_half_way() {
    let env = Env::new();
    let dir = env.root.join("ro");
    adopt_new_custom(&env, &dir);
    if !make_read_only(&dir) {
        return;
    }
    let result = env.launch();
    make_writable(&dir);
    let (_, _, why) = unavailable(result.unwrap_err());
    assert!(matches!(why, Unavailable::NotWritable(_)), "{why:?}");
}

#[test]
fn a_file_that_is_not_a_database_is_reported() {
    let env = Env::new();
    let dir = env.root.join("junk");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(DB_FILE),
        b"this is not sqlite, just text long enough",
    )
    .unwrap();
    let r = choose_data_dir(&env.dirs, &dir, NewDir::UseExisting).unwrap();
    let (_, _, why) = unavailable(prepare(&env.dirs, &r).unwrap_err());
    assert!(matches!(why, Unavailable::NotADatabase(_)), "{why:?}");
    assert!(
        env.bootstrap_bytes().is_none(),
        "a failed choice was recorded"
    );
}

#[test]
fn an_unreadable_bootstrap_is_an_error_and_is_left_as_it_was() {
    let env = Env::new();
    fs::create_dir_all(&env.dirs.app_local_data).unwrap();
    for junk in [
        &b"{ not json"[..],
        br#"{"mode":"system"}"#,
        br#"{"version":1,"mode":"custom","data_dir":"relative/path"}"#,
        br#"{"version":1,"mode":"custom","data_dir":null}"#,
        br#"{"version":1,"mode":"elsewhere","data_dir":null}"#,
    ] {
        fs::write(env.dirs.bootstrap_path(), junk).unwrap();
        let err = env.launch().unwrap_err();
        assert!(
            matches!(err, StartupError::BootstrapUnreadable { .. }),
            "{err:?}"
        );
        assert_eq!(env.bootstrap_bytes().unwrap(), junk);
        assert!(!env.dirs.system_data_dir().exists());
    }
}

#[test]
fn a_bootstrap_from_a_newer_version_is_neither_used_nor_overwritten() {
    let env = Env::new();
    fs::create_dir_all(&env.dirs.app_local_data).unwrap();
    let newer = br#"{"version":2,"mode":"cloud","where":"x"}"#;
    fs::write(env.dirs.bootstrap_path(), newer).unwrap();
    let err = env.launch().unwrap_err();
    assert!(
        matches!(err, StartupError::BootstrapUnsupported { version: 2, .. }),
        "{err:?}"
    );
    let dir = env.root.join("pc");
    fs::create_dir_all(&dir).unwrap();
    assert!(choose_data_dir(&env.dirs, &dir, NewDir::CreateNew).is_err());
    assert_eq!(env.bootstrap_bytes().unwrap(), newer);
}

#[test]
fn paths_with_spaces_and_cyrillic_survive_the_round_trip() {
    let env = Env::with(
        "Library/Application Support/Моё приложение",
        "Программы/photo cleanup",
        true,
    );
    let first = env.launch().unwrap();
    assert!(first.starts_with(&env.dirs.app_local_data));

    let dir = env.root.join("Фото архив/данные приложения");
    let db = adopt_new_custom(&env, &dir);
    assert_eq!(db, dir.join(DB_FILE));
    let b = read_bootstrap(&env.dirs.bootstrap_path()).unwrap().unwrap();
    assert_eq!(b.current.data_dir.as_deref(), Some(dir.as_path()));
    assert_eq!(marker(&env.launch().unwrap()), "custom");

    // Portable too, from a program folder with the same kind of name.
    fs::write(env.exe_dir().join(PORTABLE_MARKER), b"").unwrap();
    let portable = env.launch().unwrap();
    assert_eq!(portable, env.exe_dir().join("data").join(DB_FILE));
}

#[test]
fn a_new_archive_is_never_created_over_an_existing_one() {
    let env = Env::new();
    let dir = env.root.join("pc");
    let db = adopt_new_custom(&env, &dir);
    let err = choose_data_dir(&env.dirs, &dir, NewDir::CreateNew).unwrap_err();
    assert!(
        matches!(err, StartupError::DatabaseExists { .. }),
        "{err:?}"
    );
    assert_eq!(marker(&db), "custom");
}

#[test]
fn switching_to_a_folder_without_an_archive_is_refused() {
    let env = Env::new();
    env.launch().unwrap();
    let bootstrap = env.bootstrap_bytes();
    let dir = env.root.join("empty");
    fs::create_dir_all(&dir).unwrap();
    let (_, _, why) =
        unavailable(choose_data_dir(&env.dirs, &dir, NewDir::UseExisting).unwrap_err());
    assert_eq!(why, Unavailable::NoDatabase);
    assert!(!dir.join(DB_FILE).exists());
    assert_eq!(env.bootstrap_bytes(), bootstrap);
}

#[test]
fn relative_paths_are_refused() {
    let env = Env::new();
    let rel = Path::new("data here");
    assert!(matches!(
        choose_data_dir(&env.dirs, rel, NewDir::CreateNew),
        Err(StartupError::RelativePath { .. })
    ));
    assert!(matches!(
        resolve(&env.dirs, Some(rel)),
        Err(StartupError::RelativePath { .. })
    ));
}

#[test]
fn a_new_choice_can_be_undone_until_it_has_launched_once() {
    let env = Env::new();
    let system_db = env.launch().unwrap();
    put_marker(&system_db, "system");

    // Choose a new folder; it is recorded, but the app "restarts" before
    // the new place is confirmed — and the new place is gone by then.
    let dir = env.root.join("new place");
    fs::create_dir_all(&dir).unwrap();
    let r = choose_data_dir(&env.dirs, &dir, NewDir::CreateNew).unwrap();
    prepare(&env.dirs, &r).unwrap();
    fs::remove_dir_all(&dir).unwrap();

    let err = env.launch().unwrap_err();
    let StartupError::DataUnavailable { previous, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(previous.as_ref().unwrap().mode, StoredMode::System);

    let back = revert_to_previous(&env.dirs).unwrap();
    let p = prepare(&env.dirs, &back).unwrap();
    confirm_started(&env.dirs, &p).unwrap();
    assert_eq!(marker(&p.layout.db), "system");
    assert_eq!(marker(&env.launch().unwrap()), "system");
}

#[test]
fn a_confirmed_launch_stops_offering_the_way_back() {
    let env = Env::new();
    env.launch().unwrap();
    let dir = env.root.join("pc");
    adopt_new_custom(&env, &dir);
    let b = read_bootstrap(&env.dirs.bootstrap_path()).unwrap().unwrap();
    assert_eq!(b.previous, None);
    assert!(matches!(
        revert_to_previous(&env.dirs),
        Err(StartupError::NoPrevious)
    ));
}

#[test]
fn a_batch_file_archive_beside_the_program_is_offered_not_taken() {
    let env = Env::new();
    let legacy_db = env.exe_dir().join(DB_FILE);
    pc_db::Db::open(&legacy_db).unwrap();
    put_marker(&legacy_db, "legacy");

    let r = resolve(&env.dirs, None).unwrap();
    assert_eq!(r.legacy_dir.as_deref(), Some(env.exe_dir()));
    assert_eq!(r.source, Source::System, "taken without asking");

    // The user says "use it": that is a custom choice, nothing is copied.
    let r = choose_data_dir(&env.dirs, env.exe_dir(), NewDir::UseExisting).unwrap();
    let p = prepare(&env.dirs, &r).unwrap();
    confirm_started(&env.dirs, &p).unwrap();
    assert_eq!(p.layout.db, legacy_db);
    assert_eq!(marker(&env.launch().unwrap()), "legacy");
    assert!(!env.dirs.system_data_dir().exists());
}

#[test]
fn the_write_probe_never_replaces_a_file_already_in_the_folder() {
    let env = Env::new();
    let dir = env.root.join("override");
    fs::create_dir_all(&dir).unwrap();
    let legacy = dir.join(".photo-cleanup-write-probe");
    let payload = b"not ours \x00\xff payload".to_vec();
    fs::write(&legacy, &payload).unwrap();
    let r = resolve(&env.dirs, Some(&dir)).unwrap();
    prepare(&env.dirs, &r).unwrap();
    assert_eq!(fs::read(&legacy).unwrap(), payload);
    let probes: Vec<_> = listing(&dir)
        .into_iter()
        .filter(|n| n.starts_with(".photo-cleanup-write-probe"))
        .collect();
    assert_eq!(probes, [".photo-cleanup-write-probe"]);
}

#[test]
fn a_data_dir_override_is_used_once_and_not_remembered() {
    let env = Env::new();
    let dir = env.root.join("override");
    let r = resolve(&env.dirs, Some(&dir)).unwrap();
    assert_eq!(r.source, Source::Override);
    let p = prepare(&env.dirs, &r).unwrap();
    confirm_started(&env.dirs, &p).unwrap();
    assert!(p.layout.db.is_file());
    assert!(env.bootstrap_bytes().is_none());
    assert!(!env.dirs.app_local_data.exists());
}

#[test]
fn the_bootstrap_is_replaced_whole_and_leaves_no_temporary_file() {
    let env = Env::new();
    env.launch().unwrap();
    adopt_new_custom(&env, &env.root.join("pc"));
    assert_eq!(listing(&env.dirs.app_local_data), ["data", BOOTSTRAP_FILE]);
}

#[test]
fn errors_reach_the_shell_with_a_kind_it_can_act_on() {
    let env = Env::new();
    let dir = env.root.join("gone");
    let err = choose_data_dir(&env.dirs, &dir, NewDir::UseExisting).unwrap_err();
    let v = serde_json::to_value(&err).unwrap();
    assert_eq!(v["kind"], "data_unavailable");
    assert_eq!(v["source"], "custom");
    assert_eq!(v["why"]["what"], "missing");
    assert_eq!(v["dir"], dir.to_str().unwrap());
    assert!(err.to_string().contains("gone"));
}
