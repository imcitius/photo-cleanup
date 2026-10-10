//! A bound folder under the real server, and the files a move writes by
//! name (el-2xri, review el-146id B1–B3).
//!
//! The binding is only worth something if it governs the writes that come
//! after it: the server's own start-up (SQLite, `-wal`, migrations), every
//! later database open and writer lock, and the thumbnail cache — reset
//! included. These start the real `pc_api` server from the prepared layout,
//! exactly as the shell does, replace an object at its name the way an
//! outside program or a restored backup would, and check that the
//! replacement keeps every byte, inode, time, attribute and entry, that the
//! error says what happened, and that the genuine objects still work.
//!
//! The replacements are made by this user; the admission keeps other
//! accounts from making them (`crate::namespace`). A replacement made
//! between two system calls of the check and the write is outside what a
//! check can see — see `pc_db::Db::open_bound` for what is closed there.

use super::caller_tests::{marker, plenty, Env, THUMB};
use super::publication_tests::{own_target, signature, tree};
use super::*;
use crate::resolve::{confirm_started, prepare, resolve, revert_to_previous};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::time::Duration;

/// A moved, bound data folder and its launch, as far as `prepare`.
struct Bound {
    env: Env,
    target: PathBuf,
}

impl Bound {
    fn new() -> Self {
        let env = Env::new();
        // The server parses the settings; the fixture's marker is not JSON.
        Connection::open(&env.layout.db)
            .unwrap()
            .execute("DELETE FROM settings WHERE key='marker'", [])
            .unwrap();
        let target = own_target(&env);
        move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
        Self { env, target }
    }

    fn prepared(&self) -> crate::Prepared {
        prepare(&self.env.dirs, &resolve(&self.env.dirs, None).unwrap()).unwrap()
    }
}

/// Mark a fixture so a changed extended attribute would show in its
/// signature (macOS: `xattr`; elsewhere the bytes and times must do).
fn mark(path: &Path) {
    #[cfg(target_os = "macos")]
    assert!(std::process::Command::new("xattr")
        .args(["-w", "com.photo-cleanup.fixture", "preserve"])
        .arg(path)
        .status()
        .unwrap()
        .success());
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}

/// The error in full (`{:#}`), as the shell reports it.
async fn start(p: &crate::Prepared) -> Result<pc_api::Server, String> {
    pc_api::start(pc_api::ServerConfig {
        db_path: p.layout.db.clone(),
        thumbs: p.layout.thumbs.clone(),
        quarantine: None,
        bind: "127.0.0.1:0".parse().unwrap(),
        binding: p.storage_binding(),
    })
    .await
    .map_err(|e| format!("{e:#}"))
}

/// One ordinary request, as the window makes it.
async fn post(server: &pc_api::Server, path: &str, body: &str) -> (u16, String) {
    let addr = server.local_addr();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::task::spawn_blocking(move || {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string());
        (status, body.unwrap_or_default())
    })
    .await
    .unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// B1: between `prepare` and the server's open, the database is moved
/// aside and an empty file put at its name. The server must not start on
/// it, SQLite must not have written a byte into it (it used to grow from 0
/// to 266240 bytes before the check after start-up), and the error must
/// say so and where the proven database went.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_refuses_a_database_replaced_after_prepare_and_writes_nothing_into_it() {
    let b = Bound::new();
    let p = b.prepared();
    let boot = b.env.bootstrap();
    let db = b.target.join(DB_FILE);
    let saved = b.target.join("saved-own.db");
    fs::rename(&db, &saved).unwrap();
    fs::write(&db, []).unwrap();
    mark(&db);
    let before = tree(&b.target);
    for _ in 0..2 {
        let err = start(&p).await.unwrap_err();
        assert!(err.contains("nothing was written"), "{err}");
        assert!(err.contains("saved-own.db"), "names the proven copy: {err}");
        assert_eq!(
            tree(&b.target),
            before,
            "the replacement or its folder changed"
        );
    }
    assert!(p.verify_binding().is_err(), "the shell's classification");
    assert_eq!(b.env.bootstrap(), boot);
    // Put back, the proven database starts again.
    fs::remove_file(&db).unwrap();
    fs::rename(&saved, &db).unwrap();
    let server = start(&p).await.unwrap();
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
}

/// B1 at `prepare` itself (it opens and migrates before the server does),
/// with a plausible replacement: a copy of the real database. Its bytes
/// match; it is still another file and is not written to.
#[test]
fn prepare_refuses_a_database_replaced_after_resolve_before_opening_it() {
    let b = Bound::new();
    let r = resolve(&b.env.dirs, None).unwrap();
    let db = b.target.join(DB_FILE);
    let copy = b.target.join("copy.db");
    fs::copy(&db, &copy).unwrap();
    fs::rename(&db, b.target.join("saved-own.db")).unwrap();
    fs::rename(&copy, &db).unwrap();
    mark(&db);
    let before = tree(&b.target);
    let boot = b.env.bootstrap();
    let err = prepare(&b.env.dirs, &r).unwrap_err();
    let StartupError::DataUnavailable {
        why: crate::error::Unavailable::NotTheBoundCopy(reason),
        previous,
        ..
    } = &err
    else {
        panic!("{err:?}");
    };
    assert!(reason.contains("nothing was written"), "{reason}");
    assert!(previous.is_some(), "the way back is offered");
    assert_eq!(tree(&b.target), before);
    assert_eq!(b.env.bootstrap(), boot);
}

/// B2: the server runs on the bound folder; the thumbnail folder is then
/// replaced by another one holding somebody's file. The ordinary reset,
/// with its confirmation and writer lock, must refuse before touching
/// either the index or the replacement. The proven cache (moved aside) is
/// untouched too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_refuses_a_replaced_thumbnail_folder_and_changes_nothing() {
    let b = Bound::new();
    let p = b.prepared();
    let server = start(&p).await.unwrap();
    confirm_started(&b.env.dirs, &p).unwrap();
    let saved = b.target.join("saved-thumbs");
    fs::rename(&p.layout.thumbs, &saved).unwrap();
    fs::create_dir(&p.layout.thumbs).unwrap();
    let foreign = p.layout.thumbs.join("foreign-payload.jpg");
    fs::write(&foreign, b"foreign unique 70933").unwrap();
    mark(&foreign);
    let (before, own) = (tree(&p.layout.thumbs), tree(&saved));
    for _ in 0..2 {
        let (status, body) = post(&server, "/api/reset", r#"{"confirmation":"RESET"}"#).await;
        assert_ne!(status, 200, "{body}");
        assert!(body.contains("nothing was written"), "{body}");
        assert_eq!(tree(&p.layout.thumbs), before);
        assert_eq!(tree(&saved), own);
    }
    // A thumbnail stored now is refused too, not written into it.
    let store = pc_core::ThumbStore::bound(&p.layout.thumbs, p.storage_binding().unwrap());
    assert!(store.put(b"\xff\xd8 new thumbnail").is_err());
    assert_eq!(tree(&p.layout.thumbs), before);
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
}

/// B2, the other half: identities intact, but the bound cache itself (and
/// then the database file) made writable for everybody. The folder above
/// them being private no longer protects them once it is searchable.
#[test]
fn a_bound_cache_or_database_other_users_could_change_is_refused_at_start() {
    let b = Bound::new();
    fs::set_permissions(&b.target, fs::Permissions::from_mode(0o755)).unwrap();
    let thumbs = b.target.join(THUMBS_DIR);
    fs::set_permissions(&thumbs, fs::Permissions::from_mode(0o777)).unwrap();
    let err = resolve(&b.env.dirs, None).unwrap_err().to_string();
    assert!(err.contains("777"), "{err}");
    fs::set_permissions(&thumbs, fs::Permissions::from_mode(0o700)).unwrap();
    let db = b.target.join(DB_FILE);
    fs::set_permissions(&db, fs::Permissions::from_mode(0o666)).unwrap();
    let err = resolve(&b.env.dirs, None).unwrap_err().to_string();
    assert!(err.contains("666"), "{err}");
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(resolve(&b.env.dirs, None).is_ok());
}

/// The positive case the refusals must not cost: an ordinary reset on the
/// genuine bound objects works, keeps the binding, and the folder starts
/// again afterwards — as after a restart, and as the shell's recovery
/// restarts the server on the old folder with the same guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ordinary_reset_restart_and_recovery_work_on_the_bound_folder() {
    let b = Bound::new();
    let p = b.prepared();
    let server = start(&p).await.unwrap();
    confirm_started(&b.env.dirs, &p).unwrap();
    // A reset leaves the cache as it is: the thumbnail the store wrote and
    // the one the move copied are still there and still served.
    let store = own_store(&p);
    let key = format!("5e{}", "1".repeat(30));
    store.put_at(&key, b"own thumbnail 2417").unwrap();
    let before = untouched(&p.layout.thumbs);
    let (status, body) = post(&server, "/api/reset", r#"{"confirmation":"RESET"}"#).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""thumbnail_cache":"untouched""#), "{body}");
    assert_eq!(untouched(&p.layout.thumbs), before);
    assert_eq!(get(&server, &format!("/api/thumb/{key}")).await, 200);
    assert_eq!(own_store(&p).get(&key).unwrap(), b"own thumbnail 2417");
    assert_eq!(
        fs::read(p.layout.thumbs.join(THUMB)).unwrap(),
        vec![7u8; 5_123]
    );
    p.verify_binding().unwrap();
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
    // Recovery: the same prepared folder and guard, started again.
    let again = start(&p).await.unwrap();
    again.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
    // Restart: resolved and prepared from the bootstrap.
    let p = b.prepared();
    let server = start(&p).await.unwrap();
    p.verify_binding().unwrap();
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
}

/// A companion SQLite opens by name, replaced by a link while the server
/// runs: the next database open (every job opens one) and writer lock are
/// refused, and the linked file keeps its bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_companion_is_refused_before_the_next_write() {
    let b = Bound::new();
    let p = b.prepared();
    let server = start(&p).await.unwrap();
    let foreign = b.env.root.join("foreign-journal");
    fs::write(&foreign, b"foreign original 44120").unwrap();
    mark(&foreign);
    let journal = b.target.join(format!("{DB_FILE}-journal"));
    symlink(&foreign, &journal).unwrap();
    let before = signature(&foreign);
    let (status, body) = post(&server, "/api/reset", r#"{"confirmation":"RESET"}"#).await;
    assert_ne!(status, 200, "{body}");
    assert!(body.contains("symbolic link"), "{body}");
    let binding = p.storage_binding().unwrap();
    assert!(pc_db::Db::open_bound(&p.layout.db, binding.as_ref()).is_err());
    assert_eq!(signature(&foreign), before);
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
}

/// Recovery through "back to the previous folder" goes through the same
/// guarded open when the previous choice is bound; an unbound one (the
/// first folder) opens as before.
#[test]
fn going_back_to_the_previous_folder_still_opens_it() {
    let b = Bound::new();
    let r = revert_to_previous(&b.env.dirs).unwrap();
    let p = prepare(&b.env.dirs, &r).unwrap();
    assert_eq!(p.layout.dir, b.env.layout.dir);
}

/// B3: before the preview, the bootstrap's old fixed temporary name holds a
/// hard link, or a symbolic link, to somebody's file. The bootstrap is now
/// written through a fresh name created exclusively, so the move succeeds
/// and the linked file and the entry itself are left exactly as they were.
#[test]
fn a_link_at_the_bootstrap_temporary_name_is_neither_written_nor_removed() {
    for hard in [false, true] {
        let env = Env::new();
        let target = own_target(&env);
        let tmp = env.dirs.bootstrap_path().with_file_name("desktop.json.tmp");
        let foreign = env.root.join("foreign-bootstrap-payload");
        fs::write(&foreign, b"foreign original 68211").unwrap();
        mark(&foreign);
        if hard {
            fs::hard_link(&foreign, &tmp).unwrap();
        } else {
            symlink(&foreign, &tmp).unwrap();
        }
        let (before, entry) = (signature(&foreign), signature(&tmp));
        let preview = preview_move_with(&env.dirs, &env.layout, Source::System, &target, plenty);
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
        assert_eq!(signature(&foreign), before, "hard link: {hard}");
        assert_eq!(signature(&tmp), entry);
        assert_eq!(env.chosen(), target);
        let strays: Vec<_> = names(&env.dirs.app_local_data)
            .into_iter()
            .filter(|n| n.ends_with(".tmp") && n != "desktop.json.tmp")
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }
}

/// B3: before the preview, a file the move writes by name in the current
/// data folder — the writer lock, SQLite's `-wal` — is a link to somebody's
/// file. The preview names it; the move refuses before any lock, staging
/// folder, reservation or bootstrap write, twice, and the linked file and
/// every entry stay as they were.
#[test]
fn a_linked_source_file_is_refused_before_anything_is_written() {
    for (name, hard) in [
        (format!("{DB_FILE}.writer-lock"), false),
        (format!("{DB_FILE}.writer-lock"), true),
        (format!("{DB_FILE}-wal"), false),
        (format!("{DB_FILE}-shm"), true),
    ] {
        let env = Env::new();
        let target = own_target(&env);
        let foreign = env.root.join("foreign-payload");
        fs::write(&foreign, b"foreign original 97301").unwrap();
        mark(&foreign);
        let leaf = env.layout.dir.join(&name);
        let _ = fs::remove_file(&leaf);
        if hard {
            fs::hard_link(&foreign, &leaf).unwrap();
        } else {
            symlink(&foreign, &leaf).unwrap();
        }
        let (before, source, boot) = (signature(&foreign), tree(&env.layout.dir), env.bootstrap());
        let settings = names(&env.dirs.app_local_data);
        for _ in 0..2 {
            let preview =
                preview_move_with(&env.dirs, &env.layout, Source::System, &target, plenty);
            assert!(
                preview
                    .blockers
                    .iter()
                    .any(|b| matches!(b, Blocker::UnsafeFile { path, .. } if *path == leaf)),
                "{name}: {:?}",
                preview.blockers
            );
            let result = move_data(&env.dirs, &env.layout, Source::System, &target, plenty);
            let Err(RelocateError::Blocked { blockers }) = &result else {
                panic!("{name}: {result:?}");
            };
            let text = blockers.iter().map(ToString::to_string).collect::<String>();
            assert!(text.contains(&leaf.display().to_string()), "{text}");
            assert_eq!(signature(&foreign), before, "{name}");
            assert_eq!(tree(&env.layout.dir), source, "{name}");
            assert_eq!(env.bootstrap(), boot);
            assert_eq!(names(&env.dirs.app_local_data), settings);
            assert!(names(&target).is_empty(), "{name}: {:?}", names(&target));
        }
        // Moved away, the same move goes through.
        fs::remove_file(&leaf).unwrap();
        move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
        assert_eq!(marker(&target.join(DB_FILE)), "исходная");
        assert_eq!(signature(&foreign), before);
    }
}

/// C2 (el-5x1uh, user decision 2026-10-10): a reset does not touch the
/// thumbnail cache at all. The store's own thumbnail, the cache the move
/// copied, somebody's file at the top, a folder of somebody's, a foreign
/// name and a link under a thumbnail's name, a temporary-shaped file the
/// store never made, a file of this user's at a genuine thumbnail's name, a
/// second name of an outside file, a folder at a thumbnail's name and a
/// substituted fan-out folder 0750 all stay exactly as they were — every
/// inode, mode, owner, link count, size, modification and change time and
/// byte, folders included, so nothing was added, removed, renamed or
/// re-permissioned anywhere in the cache. The reply says the cache was not
/// touched instead of counting anything, and the thumbnail is still served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_does_not_touch_anything_in_the_thumbnail_cache() {
    use std::os::unix::fs::MetadataExt;
    let b = Bound::new();
    let p = b.prepared();
    let thumbs = p.layout.thumbs.clone();
    let store = own_store(&p);
    let key = format!("5e11{}", "2".repeat(28));
    store.put_at(&key, b"own thumbnail 3918").unwrap();
    let outside = b.env.root.join("outside-photo-3917.jpg");
    fs::write(&outside, b"outside photo 3917").unwrap();
    mark(&outside);
    fs::write(thumbs.join("notes-1204.txt"), b"notes 1204").unwrap();
    fs::create_dir(thumbs.join("album")).unwrap();
    fs::write(thumbs.join("album/IMG_5521.JPG"), b"album photo 5521").unwrap();
    fs::write(thumbs.join("5e/11/IMG_7730.JPG"), b"foreign 7730").unwrap();
    symlink(
        &outside,
        thumbs.join("5e/11/5e1199999999999999999999999999aa.jpg"),
    )
    .unwrap();
    fs::write(
        thumbs.join("ab/cd/abcd0123456789abcdef0123456789ab.5193.713.tmp"),
        b"foreign temporary-shaped payload 713",
    )
    .unwrap();
    // Thumbnails the store wrote, each moved aside and replaced at its name.
    for n in 0..3 {
        let k = store
            .put(format!("own synthetic 6381-{n}").as_bytes())
            .unwrap();
        let leaf = store.path_for(&k);
        fs::rename(&leaf, b.env.root.join(format!("own-saved-6381-{n}"))).unwrap();
        match n {
            0 => {
                fs::write(&leaf, b"foreign replacement payload 19371").unwrap();
                fs::set_permissions(&leaf, fs::Permissions::from_mode(0o640)).unwrap();
            }
            1 => fs::hard_link(&outside, &leaf).unwrap(),
            _ => {
                fs::create_dir(&leaf).unwrap();
                fs::write(leaf.join("notes"), b"foreign directory 6381").unwrap();
            }
        }
    }
    // A fan-out folder of somebody's, 0750, in the place of one the store
    // could use.
    let fan = thumbs.join("c3/d4");
    fs::create_dir_all(&fan).unwrap();
    fs::set_permissions(&fan, fs::Permissions::from_mode(0o750)).unwrap();
    fs::write(
        fan.join("c3d40123456789abcdef0123456789ab.jpg"),
        b"foreign folder shaped payload 5814",
    )
    .unwrap();
    for (path, _) in entries(&thumbs) {
        if !fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
        {
            mark(&path);
        }
    }
    let links_before = fs::metadata(&outside).unwrap().nlink();
    let outside_before = signature(&outside);
    let before = untouched(&thumbs);
    let server = start(&p).await.unwrap();
    confirm_started(&b.env.dirs, &p).unwrap();
    let (status, body) = post(&server, "/api/reset", r#"{"confirmation":"RESET"}"#).await;
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"ok": true, "thumbnail_cache": "untouched"})
    );
    assert_eq!(get(&server, &format!("/api/thumb/{key}")).await, 200);
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
    assert_eq!(untouched(&thumbs), before, "the reset changed the cache");
    assert_eq!(signature(&outside), outside_before);
    assert_eq!(fs::metadata(&outside).unwrap().nlink(), links_before);
    assert_eq!(own_store(&p).get(&key).unwrap(), b"own thumbnail 3918");
    p.verify_binding().unwrap();
}

/// Everything under `dir`, the folder itself included, links not followed:
/// kind, inode, mode, owner, link count, size, modification and change time
/// (folders too: an entry added, removed or renamed changes them) and the
/// bytes of every file. Equal before and after means nothing was touched.
fn untouched(dir: &Path) -> Vec<(PathBuf, String)> {
    use std::os::unix::fs::MetadataExt;
    entries(dir)
        .into_iter()
        .map(|(path, m)| {
            let what = if m.file_type().is_symlink() {
                format!("link {}", fs::read_link(&path).unwrap().display())
            } else if m.is_dir() {
                "dir".to_string()
            } else {
                format!("{:?}", fs::read(&path).unwrap())
            };
            let sig = format!(
                "{what} ino={} mode={:o} uid={} nlink={} len={} m={}.{} c={}.{}",
                m.ino(),
                m.mode(),
                m.uid(),
                m.nlink(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec()
            );
            (path.strip_prefix(dir).unwrap().to_path_buf(), sig)
        })
        .collect()
}

/// `dir` and everything under it, sorted, links not followed.
fn entries(dir: &Path) -> Vec<(PathBuf, fs::Metadata)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let m = fs::symlink_metadata(&d).unwrap();
        if m.is_dir() {
            for e in fs::read_dir(&d).unwrap() {
                stack.push(e.unwrap().path());
            }
        }
        out.push((d, m));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The status of one ordinary GET, as the window's `<img>` makes it.
async fn get(server: &pc_api::Server, path: &str) -> u16 {
    let addr = server.local_addr();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    tokio::task::spawn_blocking(move || {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        String::from_utf8_lossy(&out)
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0)
    })
    .await
    .unwrap()
}

/// C2: the binding accepts a held cache folder only if it is the proven
/// one — another folder, even at the proven path's place, is refused.
#[test]
fn the_binding_accepts_only_the_proven_cache_folder_as_held() {
    let b = Bound::new();
    let p = b.prepared();
    let guard = p.storage_binding().unwrap();
    let genuine = fs::File::open(&p.layout.thumbs).unwrap();
    guard.check_thumbnail_folder(&genuine).unwrap();
    let other_dir = b.target.join("other-cache");
    fs::create_dir(&other_dir).unwrap();
    let other = fs::File::open(&other_dir).unwrap();
    let err = guard.check_thumbnail_folder(&other).unwrap_err();
    assert!(err.contains("not the proven one"), "{err}");
}

/// O3 (el-5x1uh): under umask 000 the copied cache's folders are 0700 and
/// its files 0600, not 0777/0666. `umask` is process-wide, so the move
/// runs in a child copy of the test binary.
#[test]
fn under_umask_000_a_moved_cache_is_not_writable_by_others() {
    const CHILD: &str = "PC_DESKTOP_MOVE_UMASK";
    if std::env::var_os(CHILD).is_some() {
        let env = Env::new();
        Connection::open(&env.layout.db)
            .unwrap()
            .execute("DELETE FROM settings WHERE key='marker'", [])
            .unwrap();
        let target = own_target(&env);
        // The source as an older version under umask 000 left it.
        for (rel, m) in [("ab", 0o777), ("ab/cd", 0o777), (THUMB, 0o666)] {
            fs::set_permissions(env.layout.thumbs.join(rel), fs::Permissions::from_mode(m))
                .unwrap();
        }
        // SAFETY: no preconditions; this child runs only this test, on one
        // thread.
        unsafe { libc::umask(0) };
        move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
        let thumbs = target.join(THUMBS_DIR);
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&thumbs.join("ab")), 0o700);
        assert_eq!(mode(&thumbs.join("ab/cd")), 0o700);
        assert_eq!(mode(&thumbs.join(THUMB)), 0o600);
        assert_eq!(fs::read(thumbs.join(THUMB)).unwrap(), vec![7u8; 5_123]);
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "relocate::storage_tests::under_umask_000_a_moved_cache_is_not_writable_by_others",
            "--exact",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .status()
        .unwrap();
    assert!(status.success());
}

/// The thumbnail store the server itself uses for this folder: bound.
fn own_store(p: &crate::Prepared) -> pc_core::ThumbStore {
    pc_core::ThumbStore::bound(p.layout.thumbs.clone(), p.storage_binding().unwrap())
}

/// A synthetic 193×137 RGB PNG (stored deflate, no compression), so the real
/// index job has a picture to decode without any photo or image crate.
fn synthetic_png() -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let (w, h) = (193u32, 137u32);
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            raw.extend_from_slice(&[
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                ((x * 7 + y * 13) % 256) as u8,
            ]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let n = block.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    png
}

/// Run one real index job over a folder holding one synthetic picture and
/// wait for it to end: (state, error, progress).
async fn index_once(
    b: &Bound,
    p: &crate::Prepared,
    server: &pc_api::Server,
) -> (String, String, String) {
    let archive = b.env.root.join("archive-8923");
    fs::create_dir_all(&archive).unwrap();
    fs::write(archive.join("synthetic-8923.png"), synthetic_png()).unwrap();
    let body = serde_json::json!({"kind": "index", "params": {
        "roots": [archive], "min_size": 0, "workers": 1, "readers_per_disk": 1}})
    .to_string();
    let (status, response) = post(server, "/api/jobs", &body).await;
    assert_eq!(status, 202, "{response}");
    let id = serde_json::from_str::<serde_json::Value>(&response).unwrap()["job_id"]
        .as_i64()
        .unwrap();
    let conn = Connection::open(&p.layout.db).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let row: (String, String, String) = conn
            .query_row(
                "SELECT state, coalesce(error,''), coalesce(progress,'') FROM jobs WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        if !matches!(row.0.as_str(), "queued" | "running") {
            return row;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the index job did not end"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// R2 of review el-66kis: thumbnails made before a real move are served
/// from the moved cache afterwards, and an ordinary (not forced) index run
/// after the move keeps serving them. The cache has no generations: the
/// copied `ab/cd/<key>.jpg` files are the cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thumbnails_made_before_a_real_move_are_served_after_it() {
    let env = Env::new();
    Connection::open(&env.layout.db)
        .unwrap()
        .execute("DELETE FROM settings WHERE key='marker'", [])
        .unwrap();
    let target = own_target(&env);
    let b = Bound { env, target };
    let p = b.prepared();
    let server = start(&p).await.unwrap();
    let (state, error, _) = index_once(&b, &p, &server).await;
    assert_eq!(state, "done", "{error}");
    let key: String = Connection::open(&p.layout.db)
        .unwrap()
        .query_row(
            "SELECT thumb_key FROM files WHERE path LIKE '%synthetic-8923.png'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let endpoint = format!("/api/thumb/{key}");
    assert_eq!(get(&server, &endpoint).await, 200);
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();

    move_data(&b.env.dirs, &p.layout, Source::System, &b.target, plenty).unwrap();
    let moved = b.prepared();
    assert!(moved.storage_binding().is_some(), "the move did not bind");
    let server = start(&moved).await.unwrap();
    let after_move = get(&server, &endpoint).await;
    let (state, error, _) = index_once(&b, &moved, &server).await;
    assert_eq!(state, "done", "{error}");
    let after_reindex = get(&server, &endpoint).await;
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
    assert_eq!((after_move, after_reindex), (200, 200));
}

/// An ordinary file of the user's in the cache folder — at the name an
/// earlier round of this work used for a record of its own — is neither
/// appended to nor rewritten by a real index job, nor by the reset after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_file_in_the_cache_is_not_written_by_indexing_or_reset() {
    let b = Bound::new();
    let p = b.prepared();
    let server = start(&p).await.unwrap();
    confirm_started(&b.env.dirs, &p).unwrap();
    let foreign = p.layout.thumbs.join(".thumbstore-made");
    fs::write(&foreign, b"foreign notes 8923, not a cache record\n").unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    mark(&foreign);
    let before = signature(&foreign);
    let (state, error, _) = index_once(&b, &p, &server).await;
    assert_eq!(state, "done", "{error}");
    assert_eq!(
        signature(&foreign),
        before,
        "indexing wrote into the foreign file"
    );
    let (status, body) = post(&server, "/api/reset", r#"{"confirmation":"RESET"}"#).await;
    server.shutdown(pc_api::Shutdown::CancelJob).await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        signature(&foreign),
        before,
        "the reset changed the foreign file"
    );
}
