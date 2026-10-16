//! The move boundary on Windows, natively (el-6s3go, design el-61ar8 §3.2).
//!
//! On Unix another program can swap a name or a folder between the check
//! and the rename, and `binding_tests` prove the move notices. On Windows
//! the photograph is held with `DELETE` and shared for reading only, and its
//! folder and the folder it goes to are held without `FILE_SHARE_DELETE`:
//! the swap itself is refused. These tests stage another program at the
//! same moments and prove that — and every Windows-only refusal: a file in
//! use, a junction, a case-only or 8.3 collision, two spellings of one
//! sidecar, a name Windows cannot spell back, another volume, a bundle
//! with a file open inside. Synthetic files in temporary folders only.

use crate::race;
use pc_db::{Db, JournalStatus};
use pc_family::plan::Candidate;
use std::cell::Cell;
use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const ERROR_SHARING_VIOLATION: i32 = 32;

struct Archive {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    dir: PathBuf,
    quarantine: PathBuf,
    db: Db,
    run: i64,
}

fn archive() -> Archive {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let dir = root.join("archive");
    fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&root.join("test.db")).unwrap();
    let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
    Archive {
        quarantine: dir.join(pc_core::QUARANTINE_DIR),
        root,
        dir,
        _tmp: tmp,
        db,
        run,
    }
}

/// The user's own pick of `path`, indexed.
fn candidate(db: &Db, run: i64, path: &Path) -> Candidate {
    let size = fs::symlink_metadata(path).map_or(0, |m| m.len()) as i64;
    let file_id = db
        .upsert_file(
            &pc_db::NewFile {
                path: path.display().to_string(),
                name: path.file_name().unwrap().to_string_lossy().into_owned(),
                size,
                ..Default::default()
            },
            run,
        )
        .unwrap();
    Candidate {
        file_id,
        family_id: 0,
        path: path.display().to_string(),
        size,
        role: pc_family::Role::Copy,
        keeper_id: 0,
        keeper_path: String::new(),
        reason: "выбор человека".into(),
        manual: true,
        group_keeper: String::new(),
    }
}

fn last_row(db: &Db) -> (String, String) {
    db.conn
        .query_row(
            "SELECT status, coalesce(note, '') FROM journal ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn rows(db: &Db) -> i64 {
    db.conn
        .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
        .unwrap()
}

/// A junction at `at` leading to `target` (no privilege needed).
fn junction(at: &Path, target: &Path) {
    let out = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(at)
        .arg(target)
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink /J: {out:?}");
}

/// `fsutil`, which fails the test loudly rather than skip it: the CI runner
/// and the test VM both run elevated.
fn fsutil(args: &[&std::ffi::OsStr]) {
    let out = Command::new("fsutil").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "fsutil {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Another program holding `path` open for reading without sharing its
/// deletion — what Lightroom, an antivirus or Explorer's preview does.
fn held_by_another_program(path: &Path) -> fs::File {
    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
        .unwrap()
}

// ------------------------------------------------------------- P3a -------

/// A photograph another program has open is refused as in use, in the
/// words the command line and the web both show; nothing moves, and the
/// refusal is written down as a failed row with that reason, naming no
/// destination and claiming nothing moved (el-61ar8 §3.2 P3a). Once the
/// program lets go, asking again moves it.
#[test]
fn a_file_held_by_another_program_is_refused_and_named() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    let other = held_by_another_program(&photo);

    let report = crate::apply(&a.db, a.run, std::slice::from_ref(&c), None).unwrap();

    assert_eq!(report.done.frames, 0);
    let (_, why) = &report.refused[0];
    assert!(why.contains(&crate::in_use_text(&photo)), "{why}");
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    assert!(!a.quarantine.join("frame.arw").exists());
    assert_eq!(rows(&a.db), 1);
    let entry = a.db.journal_entry(1).unwrap().expect("the refusal's row");
    assert_eq!(entry.status, JournalStatus::Failed);
    assert_eq!(entry.dst, None);
    assert!(entry.manifest.is_empty(), "{:?}", entry.manifest);
    assert_eq!((entry.size, entry.file_count), (0, 0));
    let (_, note) = last_row(&a.db);
    assert!(note.contains(&crate::in_use_text(&photo)), "{note}");
    assert!(!crate::recovery::undo_offered(&entry));

    drop(other);
    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
    assert_eq!(report.done.frames, 1, "{:?}", report.refused);
    assert_eq!(
        fs::read(a.quarantine.join("frame.arw")).unwrap(),
        b"raw frame 6024"
    );
}

/// A sidecar another program has open keeps its frame home: the unit is
/// refused whole, and the row says which file and why.
#[test]
fn a_sidecar_held_by_another_program_keeps_its_frame_home() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    let side = a.dir.join("frame.xmp");
    fs::write(&photo, b"raw frame 6024").unwrap();
    fs::write(&side, b"edits 812").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    let other = held_by_another_program(&side);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
    drop(other);

    assert_eq!(report.done.frames, 0);
    let (_, why) = &report.refused[0];
    assert!(why.contains(&crate::in_use_text(&side)), "{why}");
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    assert_eq!(fs::read(&side).unwrap(), b"edits 812");
    let (status, note) = last_row(&a.db);
    assert_eq!(status, JournalStatus::Failed.as_str());
    assert!(note.contains(&side.display().to_string()), "{note}");
}

// ---------------------------------------------------------- P3c/d/g/h ----

/// While the photograph is held, another program can neither move it aside
/// nor write to it, nor move its folder or the quarantine folder: what moves
/// is the object that was checked, unchanged.
#[test]
fn a_held_photo_and_its_folders_cannot_be_moved_or_rewritten_meanwhile() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    let tried = Rc::new(Cell::new(false));
    let seen = tried.clone();
    let (dir, quarantine, root) = (a.dir.clone(), a.quarantine.clone(), a.root.clone());
    let _g = race::before_move(move |src, _| {
        let aside = src.with_file_name("aside.arw");
        let e = fs::rename(src, &aside).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{e}");
        let e = fs::OpenOptions::new().write(true).open(src).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{e}");
        assert!(fs::rename(&dir, root.join("archive-moved")).is_err());
        assert!(fs::rename(&quarantine, root.join("quarantine-moved")).is_err());
        seen.set(true);
        Ok(())
    });

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    assert!(tried.get());
    assert_eq!(report.done.frames, 1, "{:?}", report.refused);
    assert_eq!(
        fs::read(a.quarantine.join("frame.arw")).unwrap(),
        b"raw frame 6024"
    );
    assert!(!a.dir.join("aside.arw").exists());
    assert!(!a.root.join("archive-moved").exists());
}

// ---------------------------------------------------------------- P3f ----

/// What another program can still do to a held file is change its times.
/// Before the last check that is a refusal with nothing moved; right after
/// it (the one call no check covers) the arrival check catches it and the
/// photograph goes back home.
#[test]
fn times_changed_while_held_are_caught_before_and_after_the_rename() {
    for after_the_check in [false, true] {
        let a = archive();
        let photo = a.dir.join("frame.arw");
        fs::write(&photo, b"raw frame 6024").unwrap();
        let c = candidate(&a.db, a.run, &photo);
        let touch = |p: &Path| {
            let f = fs::OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(p)
                .unwrap();
            f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
                .unwrap();
        };
        let _before = (!after_the_check).then(|| {
            race::before_move(move |src, _| {
                touch(src);
                Ok(())
            })
        });
        let _after = after_the_check.then(|| {
            race::at_rename(move |at, src, _| {
                if at == race::Syscall::Before {
                    touch(src);
                }
                Ok(())
            })
        });

        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

        assert_eq!(report.done.frames, 0, "after_the_check={after_the_check}");
        let (_, why) = &report.refused[0];
        assert!(
            why.contains("modified"),
            "after_the_check={after_the_check}: {why}"
        );
        assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
        assert!(!a.quarantine.join("frame.arw").exists());
        assert!(a.db.journal_quarantined(None).unwrap().is_empty());
    }
}

// ----------------------------------------------------------------- P4 ----

/// A read-only photograph moves and comes back read-only.
#[test]
fn a_readonly_photo_moves_and_comes_back_readonly() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let readonly = |p: &Path, on: bool| {
        let mut perm = fs::metadata(p).unwrap().permissions();
        perm.set_readonly(on);
        fs::set_permissions(p, perm).unwrap();
    };
    readonly(&photo, true);
    let c = candidate(&a.db, a.run, &photo);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
    assert_eq!(report.done.frames, 1, "{:?}", report.refused);
    let held = a.quarantine.join("frame.arw");
    assert!(fs::metadata(&held).unwrap().permissions().readonly());

    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();
    crate::undo(&a.db, entry.id).unwrap();
    assert!(fs::metadata(&photo).unwrap().permissions().readonly());
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    readonly(&photo, false);
}

// ----------------------------------------------------------------- P6 ----

/// A junction at the photograph's name is held as itself and refused as a
/// link; what it leads to is never touched.
#[test]
fn a_junction_at_the_photo_name_is_refused_and_its_target_untouched() {
    let a = archive();
    let elsewhere = a.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("inside.jpg"), b"not ours 77").unwrap();
    let photo = a.dir.join("frame.jpg");
    junction(&photo, &elsewhere);
    let c = candidate(&a.db, a.run, &photo);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    assert_eq!(report.done.frames, 0);
    let (_, why) = &report.refused[0];
    assert!(why.contains("link"), "{why}");
    assert!(fs::symlink_metadata(&photo)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(elsewhere.join("inside.jpg")).unwrap(),
        b"not ours 77"
    );
    assert!(!a.quarantine.join("frame.jpg").exists());
}

/// A junction where the quarantine folder should be: nothing is moved
/// through it, and the folder it leads to stays empty.
#[test]
fn a_junction_planted_as_the_quarantine_folder_is_refused() {
    let a = archive();
    let elsewhere = a.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    junction(&a.quarantine, &elsewhere);
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let c = candidate(&a.db, a.run, &photo);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    assert_eq!(report.done.frames, 0);
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    let (status, _) = last_row(&a.db);
    assert_eq!(status, JournalStatus::Failed.as_str());
}

// ------------------------------------------------------------ P2b / P7 ---

/// A name in quarantine that differs only in case is taken, not replaced.
#[test]
fn a_case_only_collision_in_quarantine_is_refused() {
    let a = archive();
    fs::create_dir_all(&a.quarantine).unwrap();
    fs::write(a.quarantine.join("PHOTO.BMP"), b"someone else's 31").unwrap();
    let photo = a.dir.join("photo.bmp");
    fs::write(&photo, b"picture 4031").unwrap();
    let c = candidate(&a.db, a.run, &photo);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    assert_eq!(report.done.frames, 0);
    assert!(
        report.refused[0].1.contains("already exists"),
        "{:?}",
        report.refused
    );
    assert_eq!(fs::read(&photo).unwrap(), b"picture 4031");
    assert_eq!(
        fs::read(a.quarantine.join("PHOTO.BMP")).unwrap(),
        b"someone else's 31"
    );
    let names: Vec<_> = fs::read_dir(&a.quarantine)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["PHOTO.BMP"]);
}

/// A photograph whose name is the 8.3 alias of another file in quarantine:
/// the alias is taken, and the other file is not replaced. Setting a short
/// name needs an elevated token, which the CI runner and the VM have.
#[test]
fn a_short_name_alias_in_quarantine_is_refused() {
    let a = archive();
    fs::create_dir_all(&a.quarantine).unwrap();
    let other = a.quarantine.join("longer-photograph-name.jpg");
    fs::write(&other, b"someone else's 31").unwrap();
    fsutil(&[
        "file".as_ref(),
        "setshortname".as_ref(),
        other.as_os_str(),
        "LONGER~1.JPG".as_ref(),
    ]);
    let photo = a.dir.join("LONGER~1.JPG");
    fs::write(&photo, b"picture 4031").unwrap();
    let c = candidate(&a.db, a.run, &photo);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    assert_eq!(report.done.frames, 0);
    assert_eq!(fs::read(&photo).unwrap(), b"picture 4031");
    assert_eq!(fs::read(&other).unwrap(), b"someone else's 31");
}

// ----------------------------------------------------------------- T5 ----

/// In a case-sensitive folder `frame.xmp` and `frame.XMP` are two files.
/// Which is the frame's is not the tool's to guess: the whole unit is
/// refused before anything is journaled or moved.
#[test]
fn a_case_sensitive_folder_with_two_spellings_of_a_sidecar_refuses_the_unit() {
    let a = archive();
    let dir = a.dir.join("wsl");
    fs::create_dir_all(&dir).unwrap();
    fsutil(&[
        "file".as_ref(),
        "setCaseSensitiveInfo".as_ref(),
        dir.as_os_str(),
        "enable".as_ref(),
    ]);
    let photo = dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    fs::write(dir.join("frame.xmp"), b"edits one").unwrap();
    fs::write(dir.join("frame.XMP"), b"edits two").unwrap();
    let c = candidate(&a.db, a.run, &photo);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    assert_eq!(report.done.frames, 0);
    let (_, why) = &report.refused[0];
    assert!(why.contains("differ only in letter case"), "{why}");
    assert!(
        why.contains("frame.xmp") && why.contains("frame.XMP"),
        "{why}"
    );
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    assert_eq!(fs::read(dir.join("frame.xmp")).unwrap(), b"edits one");
    assert_eq!(fs::read(dir.join("frame.XMP")).unwrap(), b"edits two");
    assert_eq!(rows(&a.db), 0);
}

/// `icacls`, failing the test loudly like [`fsutil`].
fn icacls(args: &[&std::ffi::OsStr]) {
    let out = Command::new("icacls").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "icacls {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A folder the current user may not list, given back on drop so the
/// temporary folder can be cleaned up.
struct Unlistable {
    dir: PathBuf,
    user: String,
}

impl Unlistable {
    fn deny(dir: &Path) -> Unlistable {
        let out = Command::new("whoami").output().unwrap();
        assert!(out.status.success());
        let user = String::from_utf8_lossy(&out.stdout).trim().to_string();
        icacls(&[
            dir.as_os_str(),
            "/deny".as_ref(),
            format!("{user}:(RD)").as_ref(),
        ]);
        Unlistable {
            dir: dir.to_path_buf(),
            user,
        }
    }
}

impl Drop for Unlistable {
    fn drop(&mut self) {
        let _ = Command::new("icacls")
            .arg(&self.dir)
            .args(["/remove:d", &self.user])
            .output();
    }
}

/// Companions are found by listing the folder. A folder that cannot be
/// listed may hide a sidecar, so the frame is refused rather than moved
/// without it — and nothing is journaled or moved.
#[test]
fn a_folder_that_cannot_be_listed_keeps_the_frame_and_its_sidecars_home() {
    let a = archive();
    let dir = a.dir.join("locked");
    fs::create_dir_all(&dir).unwrap();
    let photo = dir.join("frame.arw");
    fs::write(&photo, b"raw frame 7311").unwrap();
    fs::write(dir.join("frame.xmp"), b"edits 7311").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    let denied = Unlistable::deny(&dir);
    assert!(fs::read_dir(&dir).is_err(), "the folder is still listable");

    let why = crate::files::companion_doubt(&photo).expect("refused");
    assert!(why.contains("could not be listed in full"), "{why}");
    // Whether the refusal comes from the listing or from holding the
    // folder first, nothing may move.
    if let Ok(report) = crate::apply(&a.db, a.run, &[c], None) {
        assert_eq!(report.done.frames, 0);
        assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    }
    drop(denied);
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 7311");
    assert_eq!(fs::read(dir.join("frame.xmp")).unwrap(), b"edits 7311");
    assert_eq!(rows(&a.db), 0);
}

/// In an ordinary folder a sidecar spelled `FRAME.XMP` travels and comes
/// back under exactly that spelling.
#[test]
fn a_sidecar_keeps_its_own_spelling_there_and_back() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    fs::write(a.dir.join("frame.XMP"), b"edits").unwrap();
    let c = candidate(&a.db, a.run, &photo);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
    assert_eq!(report.done.companions, 1, "{:?}", report.refused);
    let names = |d: &Path| -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    assert_eq!(names(&a.quarantine), ["frame.XMP", "frame.arw"]);

    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();
    crate::undo(&a.db, entry.id).unwrap();
    assert!(names(&a.dir).contains(&"frame.XMP".to_string()));
}

// ----------------------------------------------------------------- P8 ----

/// A photograph deeper than MAX_PATH moves and comes back.
#[test]
fn a_photo_deeper_than_max_path_moves_and_returns() {
    let a = archive();
    let mut deep = a.dir.clone();
    while deep.as_os_str().len() < 300 {
        deep.push("a-folder-of-forty-characters-0123456789");
    }
    fs::create_dir_all(&deep).unwrap();
    let photo = deep.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    assert!(photo.as_os_str().len() > 300);
    let c = candidate(&a.db, a.run, &photo);

    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
    assert_eq!(report.done.frames, 1, "{:?}", report.refused);
    assert!(!photo.exists());

    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();
    crate::undo(&a.db, entry.id).unwrap();
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
}

// ---------------------------------------------------------------- P11 ----

/// A configured quarantine on another volume is refused before anything is
/// written there or moved. The CI runner keeps its temporary folder on C:
/// and the build on D:; elsewhere a folder on a second volume can be named
/// in `PC_TEST_OTHER_VOLUME`. Without one this skips, except on CI.
#[test]
fn a_quarantine_on_another_volume_is_refused_before_anything_moves() {
    let a = archive();
    let other_root = std::env::var_os("PC_TEST_OTHER_VOLUME")
        .map(PathBuf::from)
        .or_else(|| Some(std::env::current_exe().ok()?.parent()?.to_path_buf()))
        .unwrap();
    let other = tempfile::tempdir_in(&other_root).unwrap();
    let serial = |p: &Path| pc_core::anchored::Dir::open(p).unwrap().ident().unwrap().0;
    if serial(other.path()) == serial(&a.dir) {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must have a second volume for this test"
        );
        eprintln!("skipped: {} is on the same volume", other_root.display());
        return;
    }
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let c = candidate(&a.db, a.run, &photo);

    let e = crate::apply(&a.db, a.run, &[c], Some(other.path())).unwrap_err();

    assert!(format!("{e:#}").contains("different filesystem"), "{e:#}");
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
    assert_eq!(rows(&a.db), 0);
}

// ----------------------------------------------------------------- T2 ----

/// A configured quarantine whose path holds a name Windows cannot spell
/// back (a trailing dot, a device name) is refused: nothing is created.
#[test]
fn a_quarantine_named_in_a_way_windows_cannot_spell_back_is_refused() {
    for bad in ["q.", "CON", "q "] {
        let a = archive();
        let photo = a.dir.join("frame.arw");
        fs::write(&photo, b"raw frame 6024").unwrap();
        let c = candidate(&a.db, a.run, &photo);
        let root = a.root.join(bad);

        let r = crate::apply(&a.db, a.run, &[c], Some(&root));

        let refused = match &r {
            Err(_) => true,
            Ok(report) => report.done.frames == 0 && !report.refused.is_empty(),
        };
        assert!(refused, "{bad:?}: {r:?}");
        assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024", "{bad:?}");
        let mut left: Vec<String> = fs::read_dir(&a.root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with("test.db"))
            .collect();
        left.sort();
        assert_eq!(left, ["archive"], "{bad:?}: something was created");
    }
}

// ----------------------------------------------------------------- T4b ---

/// A bundle an earlier version put in quarantine, with a file inside it
/// open in another program: Windows will not rename the folder, the undo
/// says why in both possible words, and the entry stays open. Asked again
/// once the file is closed, it comes home.
#[test]
fn a_bundle_with_an_open_file_inside_is_refused_and_says_why() {
    let a = archive();
    let dir = a.dir.join("Library Previews.lrdata");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("cache"), b"cached 512").unwrap();
    a.db.upsert_bundle(
        &pc_db::model::NewBundle {
            path: dir.display().to_string(),
            is_dir: true,
            disk: "root".into(),
            dev: 0,
            mount: a.dir.display().to_string(),
            kind: pc_core::DerivedKind::LrPreviews,
            owner_ref: None,
            file_count: 1,
            size: 10,
            newest_mtime: 0,
        },
        a.run,
    )
    .unwrap();
    let b =
        a.db.list_bundles(&Default::default())
            .unwrap()
            .pop()
            .unwrap();
    let id = crate::legacy::moved_by_an_earlier_version(&a.db, a.run, &b).unwrap();
    let held = a.quarantine.join("Library Previews.lrdata");
    let inside = fs::File::open(held.join("cache")).unwrap();

    let e = crate::undo(&a.db, id).unwrap_err();
    drop(inside);

    let shown = format!("{e:#}");
    assert!(
        shown.contains("a file inside the folder is open in another program"),
        "{shown}"
    );
    assert!(!dir.exists());
    assert_eq!(fs::read(held.join("cache")).unwrap(), b"cached 512");
    assert_eq!(
        a.db.journal_entry(id).unwrap().unwrap().status,
        JournalStatus::Done
    );

    crate::undo(&a.db, id).unwrap();
    assert_eq!(fs::read(dir.join("cache")).unwrap(), b"cached 512");
}

// ----------------------------------------------------------------- R3 ----

/// Windows deletes nothing: purge is refused for every entry, and what is
/// in quarantine stays there and can still be undone.
#[test]
fn windows_never_purges() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    crate::apply(&a.db, a.run, &[c], None).unwrap();
    let held = a.quarantine.join("frame.arw");
    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();

    // Past the holding period: a cutoff in the future takes every entry.
    let totals = crate::purge(&a.db, -3600).unwrap();
    assert_eq!((totals.files, totals.bytes), (0, 0));
    assert!(
        totals.skipped[0].contains("on Windows permanent deletion is not carried out"),
        "{:?}",
        totals.skipped
    );
    let e = crate::purge_entry(&a.db, entry.id).unwrap_err();
    assert!(format!("{e:#}").contains("on Windows"), "{e:#}");
    assert_eq!(fs::read(&held).unwrap(), b"raw frame 6024");

    crate::undo(&a.db, entry.id).unwrap();
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
}

// --------------------------------------------------------- el-61ar8 §4.C ---

/// A reorganisation move of `src` to `dst`, as the planner hands it over:
/// indexed, with the size and time the index recorded.
fn organized(a: &Archive, src: &Path, dst: &Path) -> pc_organize::Move {
    let md = fs::metadata(src).unwrap();
    let mtime = pc_core::time::mtime_unix(&md);
    let file_id =
        a.db.upsert_file(
            &pc_db::NewFile {
                path: src.display().to_string(),
                name: src.file_name().unwrap().to_string_lossy().into_owned(),
                size: md.len() as i64,
                mtime,
                ..Default::default()
            },
            a.run,
        )
        .unwrap();
    pc_organize::Move {
        file_id,
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        size: md.len() as i64,
        mtime,
        date: pc_organize::Dated {
            ts: 1_562_000_000,
            source: pc_organize::Source::Exif,
            precision: pc_organize::Precision::Day,
        },
        event: "2019".into(),
        renamed_from: None,
    }
}

fn indexed_path(db: &Db, file_id: i64) -> String {
    db.conn
        .query_row("SELECT path FROM files WHERE id = ?1", [file_id], |r| {
            r.get(0)
        })
        .unwrap()
}

/// A reorganisation on NTFS: the frame goes to its new folder with its
/// sidecar, the index follows, and the service file left alone in the
/// emptied folder is moved into quarantine beside it — moved, never
/// deleted, and the emptied folder stays. Walking the run back brings every
/// one of them home, and the index with them.
#[test]
fn a_reorganisation_moves_the_frame_its_sidecar_and_the_service_files_and_comes_back() {
    let a = archive();
    let old = a.dir.join("card");
    fs::create_dir_all(&old).unwrap();
    let photo = old.join("DSC01234.ARW");
    let side = old.join("DSC01234.xmp");
    let finder = old.join(".DS_Store");
    fs::write(&photo, b"raw frame 6024").unwrap();
    fs::write(&side, b"edits 812").unwrap();
    fs::write(&finder, b"finder 6148").unwrap();
    let new = a.dir.join("2019").join("2019-07-01");
    let m = organized(&a, &photo, &new.join("DSC01234.ARW"));
    let id = m.file_id;

    crate::check_organize(std::slice::from_ref(&m)).unwrap();
    let report = crate::organize(&a.db, a.run, &[m]).unwrap();

    assert_eq!(
        (
            report.done.frames,
            report.done.companions,
            report.done.litter
        ),
        (1, 1, 1),
        "{:?}",
        report.refused
    );
    assert_eq!(
        fs::read(new.join("DSC01234.ARW")).unwrap(),
        b"raw frame 6024"
    );
    assert_eq!(fs::read(new.join("DSC01234.xmp")).unwrap(), b"edits 812");
    let swept = a.quarantine.join("card").join(".DS_Store");
    assert_eq!(fs::read(&swept).unwrap(), b"finder 6148");
    assert!(old.is_dir(), "the emptied folder was removed");
    assert_eq!(fs::read_dir(&old).unwrap().count(), 0);
    assert_eq!(
        indexed_path(&a.db, id),
        new.join("DSC01234.ARW").display().to_string()
    );

    let (back, failed) = crate::undo_run(&a.db, a.run).unwrap();

    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(back.entries_back, 2);
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    assert_eq!(fs::read(&side).unwrap(), b"edits 812");
    assert_eq!(fs::read(&finder).unwrap(), b"finder 6148");
    assert!(fs::symlink_metadata(new.join("DSC01234.ARW")).is_err());
    assert!(fs::symlink_metadata(&swept).is_err());
    assert_eq!(indexed_path(&a.db, id), photo.display().to_string());
}

/// A photograph another program has open is not reorganised: it is refused
/// as in use, in the words the command line and the web both show, the
/// refusal is a failed row, and nothing moves. Once it is closed, it moves.
#[test]
fn a_reorganisation_of_a_file_held_by_another_program_is_refused_and_named() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let dst = a.dir.join("2019").join("frame.arw");
    let m = organized(&a, &photo, &dst);
    let other = held_by_another_program(&photo);

    let report = crate::organize(&a.db, a.run, std::slice::from_ref(&m)).unwrap();

    assert_eq!(report.done.frames, 0);
    let (_, why) = &report.refused[0];
    assert!(why.contains(&crate::in_use_text(&photo)), "{why}");
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    assert!(fs::symlink_metadata(&dst).is_err());
    let (status, note) = last_row(&a.db);
    assert_eq!(status, JournalStatus::Failed.as_str());
    assert!(note.contains(&crate::in_use_text(&photo)), "{note}");
    assert!(a
        .db
        .journal_by_run_op(a.run, "organize")
        .unwrap()
        .is_empty());

    drop(other);
    let report = crate::organize(&a.db, a.run, &[m]).unwrap();
    assert_eq!(report.done.frames, 1, "{:?}", report.refused);
    assert_eq!(fs::read(&dst).unwrap(), b"raw frame 6024");
}

/// A reorganisation into a folder on another volume is refused before
/// anything moves: the preview's check and the run say so. (As
/// `a_quarantine_on_another_volume_is_refused_before_anything_moves`.)
#[test]
fn a_reorganisation_onto_another_volume_is_refused_before_anything_moves() {
    let a = archive();
    let other_root = std::env::var_os("PC_TEST_OTHER_VOLUME")
        .map(PathBuf::from)
        .or_else(|| Some(std::env::current_exe().ok()?.parent()?.to_path_buf()))
        .unwrap();
    let other = tempfile::tempdir_in(&other_root).unwrap();
    let serial = |p: &Path| pc_core::anchored::Dir::open(p).unwrap().ident().unwrap().0;
    if serial(other.path()) == serial(&a.dir) {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must have a second volume for this test"
        );
        eprintln!("skipped: {} is on the same volume", other_root.display());
        return;
    }
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let m = organized(&a, &photo, &other.path().join("2019").join("frame.arw"));

    let checked = crate::check_organize(std::slice::from_ref(&m)).unwrap_err();
    let e = crate::organize(&a.db, a.run, &[m]).unwrap_err();

    for e in [checked, e] {
        assert!(format!("{e:#}").contains("different filesystem"), "{e:#}");
    }
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
    assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
    assert_eq!(rows(&a.db), 0);
}

/// Set `at`'s last-write time to `like`'s: the only field of the evidence a
/// stranger could share besides its size, so what tells them apart is the
/// NTFS file ID alone.
fn same_times(at: &Path, like: &Path) {
    let t = fs::metadata(like).unwrap().modified().unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(at)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// The process dies between the rename and the journal's record of it, in
/// a move into quarantine and in a reorganisation: the row is left
/// `pending`. Someone moves the frame aside and puts a copy of it in its
/// place — the same bytes, the same size, the same last-write time — so
/// only the NTFS file ID tells them apart. The reconciliation does not take
/// the copy for the frame: nothing moves, the copy stays, the row stays
/// pending and says where. With the frame itself back at that place, the
/// reconciliation brings it home by its file ID and closes the row.
#[test]
fn a_crash_between_the_rename_and_the_journal_is_reconciled_by_file_id_and_claims_no_stranger() {
    for organize in [false, true] {
        let a = archive();
        let photo = a.dir.join("frame.arw");
        fs::write(&photo, b"raw frame 6024").unwrap();
        let held = if organize {
            a.dir.join("2019").join("frame.arw")
        } else {
            a.quarantine.join("frame.arw")
        };
        a.db.conn
            .execute_batch(
                "CREATE TRIGGER crash BEFORE UPDATE OF status ON journal
                 BEGIN SELECT RAISE(ABORT, 'process killed'); END;",
            )
            .unwrap();
        if organize {
            let m = organized(&a, &photo, &held);
            assert!(crate::organize(&a.db, a.run, &[m]).is_err());
        } else {
            let c = candidate(&a.db, a.run, &photo);
            let _ = crate::files::quarantine_file(&a.db, a.run, &c, None);
        }
        a.db.conn.execute_batch("DROP TRIGGER crash").unwrap();
        let id: i64 =
            a.db.conn
                .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
                .unwrap();
        let entry = a.db.journal_entry(id).unwrap().unwrap();
        assert_eq!(entry.status, JournalStatus::Pending, "organize={organize}");
        assert_eq!(fs::read(&held).unwrap(), b"raw frame 6024");
        assert!(fs::symlink_metadata(&photo).is_err());
        let ours = entry.manifest[0].proof.clone().expect("evidence recorded");

        // The frame aside; a copy of it, to the last-write time, in its place.
        let aside = a.dir.join("frame-aside.arw");
        fs::rename(&held, &aside).unwrap();
        fs::write(&held, b"raw frame 6024").unwrap();
        same_times(&held, &aside);
        let copy = crate::files::evidence(&held).unwrap().unwrap();
        assert_eq!(
            (copy.dev, copy.size, copy.mtime_ns),
            (ours.dev, ours.size, ours.mtime_ns),
            "organize={organize}: the copy differs in more than its file ID"
        );
        assert_ne!(copy.ino, ours.ino);

        let e = crate::reconcile_undo(&a.db, id).unwrap_err();

        let shown = format!("{e:#}");
        assert!(
            shown.contains(&held.display().to_string()),
            "organize={organize}: {shown}"
        );
        assert!(
            fs::symlink_metadata(&photo).is_err(),
            "a stranger came home"
        );
        assert_eq!(fs::read(&held).unwrap(), b"raw frame 6024");
        assert_eq!(fs::read(&aside).unwrap(), b"raw frame 6024");
        let (status, note) = last_row(&a.db);
        assert_eq!(status, "pending", "organize={organize}");
        assert!(note.contains(&held.display().to_string()), "{note}");

        // The copy is the user's to keep; the frame goes back where the
        // operation put it, and comes home from there.
        let kept = a.dir.join("copy-kept.arw");
        fs::rename(&held, &kept).unwrap();
        fs::rename(&aside, &held).unwrap();

        let r = crate::reconcile_undo(&a.db, id).unwrap();

        assert_eq!(r.done.files_back, 1, "organize={organize}");
        assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
        assert!(fs::symlink_metadata(&held).is_err());
        assert_eq!(fs::read(&kept).unwrap(), b"raw frame 6024");
        assert_eq!(
            a.db.journal_entry(id).unwrap().unwrap().status,
            JournalStatus::Undone
        );
    }
}

/// A file in quarantine the journal never recorded comes back where its
/// path says, and that return is journaled and can be walked back like any
/// other move. A junction at an orphan's path is not followed: it is
/// refused, and it and what it leads to stay as they were.
#[test]
fn an_orphan_comes_home_and_a_junction_in_its_place_is_refused() {
    let a = archive();
    fs::create_dir_all(&a.quarantine).unwrap();
    let orphan = a.quarantine.join("orphan.arw");
    fs::write(&orphan, b"nobody's 9").unwrap();
    let back = a.dir.join("orphan.arw");

    let done = crate::adopt_orphan(
        &a.db,
        a.run,
        &orphan.display().to_string(),
        &back.display().to_string(),
    )
    .unwrap();

    assert_eq!(done.files_back, 1);
    assert_eq!(fs::read(&back).unwrap(), b"nobody's 9");
    assert!(fs::symlink_metadata(&orphan).is_err());
    let entry =
        a.db.journal_by_run_op(a.run, "adopt")
            .unwrap()
            .pop()
            .unwrap();
    assert_eq!(entry.status, JournalStatus::Done);
    crate::undo(&a.db, entry.id).unwrap();
    assert_eq!(fs::read(&orphan).unwrap(), b"nobody's 9");
    assert!(fs::symlink_metadata(&back).is_err());

    let elsewhere = a.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("inside.jpg"), b"the user's 21").unwrap();
    let link = a.quarantine.join("Album");
    junction(&link, &elsewhere);
    let home = a.dir.join("Album");
    let rows_before = rows(&a.db);

    let r = crate::adopt_orphan(
        &a.db,
        a.run,
        &link.display().to_string(),
        &home.display().to_string(),
    );

    assert!(r.is_err(), "{r:?}");
    assert_eq!(rows(&a.db), rows_before, "a refused orphan wrote a row");
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(&home).is_err());
    assert_eq!(
        fs::read(elsewhere.join("inside.jpg")).unwrap(),
        b"the user's 21"
    );
}

// ----------------------------------------------------------- gathered ----

/// A gathered quarantine on the same NTFS volume: the layout note is
/// written through the held folder, the photograph lands under its disk
/// label, and comes back.
#[test]
fn a_gathered_quarantine_is_written_and_walked_back() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    let root = a.root.join("gathered");

    let report = crate::apply(&a.db, a.run, &[c], Some(&root)).unwrap();
    assert_eq!(report.done.frames, 1, "{:?}", report.refused);
    // It really left: once the destination was built from the whole source
    // path, and the photograph was "moved" onto its own name.
    assert!(!photo.exists(), "{}", photo.display());
    let home = root.join(pc_core::QUARANTINE_DIR);
    let disks = pc_core::quarantine_layout::read(&home);
    assert_eq!(disks.len(), 1, "{disks:?}");

    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();
    let dst = PathBuf::from(entry.dst.as_deref().unwrap());
    assert!(dst.starts_with(&home), "{}", dst.display());
    assert_eq!(fs::read(&dst).unwrap(), b"raw frame 6024");
    crate::undo(&a.db, entry.id).unwrap();
    assert_eq!(fs::read(&photo).unwrap(), b"raw frame 6024");
}

/// The paths of the index and of the mount differ in spelling on Windows —
/// verbatim `\\?\C:\` from `canonicalize`, `c:` in lower case — and the
/// gathered quarantine still mirrors the path below the drive, never the
/// whole source path (el-6s3go: that once made the destination the source
/// itself).
#[test]
fn a_gathered_destination_mirrors_the_path_below_the_drive_however_spelled() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw frame 6024").unwrap();
    let root = a.root.join("gathered");
    let canonical = photo.canonicalize().unwrap();
    let spelled = photo.display().to_string();
    let lower = format!("{}{}", spelled[..1].to_lowercase(), &spelled[1..]);
    for src in [spelled.clone(), canonical.display().to_string(), lower] {
        let dst = crate::quarantine_dest_for(&src, 0, &a.db, Some(&root)).unwrap();
        assert!(
            dst.starts_with(root.join(pc_core::QUARANTINE_DIR)),
            "{src}: {}",
            dst.display()
        );
        assert_ne!(dst, PathBuf::from(&src));
        assert!(dst.ends_with(r"archive\frame.arw"), "{}", dst.display());
    }
}
