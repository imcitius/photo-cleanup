//! Something appears at a destination after the last look at it (el-usdqi).
//!
//! Every move in this crate looked at the destination and then renamed onto
//! it. Between the two, Lightroom, a sync client, a Photos import or a second
//! tool can create a file there — and a plain `rename` replaces it without a
//! word. `exists()` does not even see a dangling symlink. These tests stage
//! that moment through [`crate::race`] on the real consumers: apply into
//! quarantine, the undo back into the archive, and the reorganisation.

use crate::race;
use pc_db::{Db, JournalStatus};
use pc_family::plan::Candidate;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[path = "../../pc-core/src/derived/fixtures.rs"]
mod junk;

/// What turns up at the destination.
#[derive(Clone, Copy, Debug)]
enum Stranger {
    /// A file with someone else's bytes.
    File,
    /// A symlink that leads nowhere: `exists()` says there is nothing.
    Dangling,
    /// An empty directory (what a plain `rename` of a directory replaces).
    EmptyDir,
}

const STRANGER: &[u8] = b"someone else's photograph";

fn plant(at: &Path, what: Stranger) {
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    match what {
        Stranger::File => fs::write(at, STRANGER).unwrap(),
        Stranger::Dangling => link(&at.with_file_name("nowhere"), at),
        Stranger::EmptyDir => fs::create_dir(at).unwrap(),
    }
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// A symlink on Windows needs a privilege CI runners do not have.
#[cfg(not(unix))]
fn link(_: &Path, _: &Path) {
    unreachable!("a dangling symlink is planted on Unix only")
}

/// The strangers a file destination can meet on this system.
fn strangers() -> Vec<Stranger> {
    let mut all = vec![Stranger::File];
    if cfg!(unix) {
        all.push(Stranger::Dangling);
    }
    all
}

/// The stranger is still there, exactly as planted.
fn intact(at: &Path, what: Stranger) {
    let md = fs::symlink_metadata(at).unwrap_or_else(|e| panic!("{}: {e}", at.display()));
    match what {
        Stranger::File => assert_eq!(fs::read(at).unwrap(), STRANGER, "{}", at.display()),
        Stranger::Dangling => {
            assert!(md.file_type().is_symlink(), "{} заменена", at.display());
            assert_eq!(fs::read_link(at).unwrap(), at.with_file_name("nowhere"));
        }
        Stranger::EmptyDir => {
            assert!(md.is_dir(), "{} заменён", at.display());
            assert_eq!(fs::read_dir(at).unwrap().count(), 0, "{}", at.display());
        }
    }
}

/// Plant `what` at `at` the first time something is about to move there.
fn race_at(at: PathBuf, what: Stranger) -> race::Guard {
    let mut done = false;
    race::before_move(move |_, dst| {
        if !done && dst == at {
            done = true;
            plant(&at, what);
        }
        Ok(())
    })
}

fn journal_row(db: &Db, id: i64) -> (String, String) {
    db.conn
        .query_row(
            "SELECT status, coalesce(note, '') FROM journal WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn last_journal_id(db: &Db) -> i64 {
    db.conn
        .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
        .unwrap()
}

/// A photograph in the index, and the manual candidate that moves it.
fn candidate(db: &Db, run: i64, path: &Path) -> Candidate {
    let size = fs::metadata(path).unwrap().len() as i64;
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

struct Archive {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    quarantine: PathBuf,
    db: Db,
    run: i64,
}

fn archive() -> Archive {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("archive");
    fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&tmp.path().join("test.db")).unwrap();
    let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
    Archive {
        quarantine: dir.join(pc_core::QUARANTINE_DIR),
        dir,
        _tmp: tmp,
        db,
        run,
    }
}

fn file_state(db: &Db, path: &Path) -> String {
    db.conn
        .query_row(
            "SELECT state FROM files WHERE path = ?1",
            [path.display().to_string()],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn apply_never_replaces_what_appears_in_quarantine() {
    for what in strangers() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        fs::write(&photo, b"picture").unwrap();
        let c = candidate(&a.db, a.run, &photo);
        let dst = a.quarantine.join("photo.bmp");

        let _race = race_at(dst.clone(), what);
        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

        assert_eq!(report.done.frames, 0, "перенос поверх чужого файла");
        let (_, why) = &report.refused[0];
        assert!(why.contains(&dst.display().to_string()), "{why}");
        assert_eq!(fs::read(&photo).unwrap(), b"picture");
        intact(&dst, what);
        let (status, note) = journal_row(&a.db, last_journal_id(&a.db));
        assert_eq!(status, JournalStatus::Failed.as_str());
        assert!(note.contains(&dst.display().to_string()), "{note}");
        assert_eq!(file_state(&a.db, &photo), "present");
    }
}

#[test]
fn a_sidecar_never_replaces_what_appears_beside_its_photograph() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"raw").unwrap();
    fs::write(a.dir.join("frame.xmp"), b"my edits").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    let taken = a.quarantine.join("frame.xmp");

    let _race = race_at(taken.clone(), Stranger::File);
    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    // The sidecar could not follow, so the photograph was put back: the
    // frame and its companions move as one (user decision (c)).
    assert_eq!(report.done.frames, 0);
    let (_, stuck) = &report.refused[0];
    assert!(stuck.contains("frame.xmp"), "{stuck}");
    intact(&taken, Stranger::File);
    assert_eq!(fs::read(&photo).unwrap(), b"raw");
    assert_eq!(fs::read(a.dir.join("frame.xmp")).unwrap(), b"my edits");
    assert!(!a.quarantine.join("frame.arw").exists());
    assert!(a.db.journal_quarantined(None).unwrap().is_empty());
}

fn bundle(a: &Archive, path: &Path, is_dir: bool) -> pc_db::Bundle {
    a.db.upsert_bundle(
        &pc_db::model::NewBundle {
            path: path.display().to_string(),
            is_dir,
            disk: "root".into(),
            dev: 0,
            mount: a.dir.display().to_string(),
            kind: pc_core::DerivedKind::SystemJunk,
            owner_ref: None,
            file_count: 1,
            size: 0,
            newest_mtime: pc_core::time::mtime_unix(&fs::metadata(path).unwrap()),
        },
        a.run,
    )
    .unwrap();
    a.db.list_bundles(&Default::default())
        .unwrap()
        .into_iter()
        .find(|b| b.path == path.display().to_string())
        .unwrap()
}

/// el-wda81 B2 and el-2rpxq B2-R2: a companion an earlier version recorded
/// is left and named as a companion whether its file is gone, there, or
/// there in the other Unicode normal form; nothing moves and nothing is
/// journalled.
#[test]
fn a_companion_is_left_and_named_with_or_without_its_file() {
    let a = archive();
    for (sat, frame) in [
        ("._frame.jpg", None),
        ("._frame2.jpg", Some("frame2.jpg")),
        ("._cafe\u{301}.png", Some("caf\u{e9}.png")),
        ("gone.png@SynoEAStream", None),
    ] {
        let sat = a.dir.join(sat);
        fs::write(&sat, junk::apple_double()).unwrap();
        if let Some(f) = frame {
            fs::write(a.dir.join(f), b"synthetic frame").unwrap();
        }
        let b = bundle(&a, &sat, false);
        let sel = crate::select_derived(&a.db, &[b.kind], None).unwrap();
        let (_, why) = sel.excluded.iter().find(|(p, _)| *p == b.path).unwrap();
        assert!(why.contains("companion"), "{why}");
        assert_eq!(fs::read(&sat).unwrap(), junk::apple_double());
    }
    assert!(!a.quarantine.exists());
    assert!(a.db.journal_quarantined(None).unwrap().is_empty());
}

#[test]
fn an_undo_never_replaces_what_appears_at_home() {
    for what in strangers() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        fs::write(&photo, b"picture").unwrap();
        let c = candidate(&a.db, a.run, &photo);
        crate::apply(&a.db, a.run, &[c], None).unwrap();
        let held = a.quarantine.join("photo.bmp");
        let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();

        let race = race_at(photo.clone(), what);
        let err = crate::undo(&a.db, entry.id).unwrap_err().to_string();
        drop(race);

        assert!(err.contains(&photo.display().to_string()), "{err}");
        intact(&photo, what);
        assert_eq!(fs::read(&held).unwrap(), b"picture");
        // Still the entry that can bring it back, with the reason on it.
        let (status, note) = journal_row(&a.db, entry.id);
        assert_eq!(status, JournalStatus::Done.as_str());
        assert!(note.contains(&photo.display().to_string()), "{note}");
        assert_eq!(a.db.journal_quarantined(None).unwrap().len(), 1);
    }
}

#[test]
fn an_undo_never_replaces_a_sidecar_that_appears_at_home_and_can_be_retried() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    let side = a.dir.join("frame.xmp");
    fs::write(&photo, b"raw").unwrap();
    fs::write(&side, b"my edits").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    crate::apply(&a.db, a.run, &[c], None).unwrap();
    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();
    assert_eq!(entry.manifest.len(), 2);

    let race = race_at(side.clone(), Stranger::File);
    let err = crate::undo(&a.db, entry.id).unwrap_err().to_string();
    drop(race);

    // No half undo: the photograph went back into quarantine with its
    // sidecar (user decision (c)).
    assert!(err.contains("frame.xmp"), "{err}");
    assert!(!photo.exists(), "половина отката");
    assert_eq!(fs::read(a.quarantine.join("frame.arw")).unwrap(), b"raw");
    intact(&side, Stranger::File);
    assert_eq!(
        fs::read(a.quarantine.join("frame.xmp")).unwrap(),
        b"my edits"
    );
    let (status, _) = journal_row(&a.db, entry.id);
    assert_eq!(status, JournalStatus::Done.as_str(), "половина отката");

    // The person moves the stranger away; asking again finishes the job.
    fs::rename(&side, a.dir.join("stranger.xmp")).unwrap();
    crate::undo(&a.db, entry.id).unwrap();
    assert_eq!(fs::read(&photo).unwrap(), b"raw");
    assert_eq!(fs::read(&side).unwrap(), b"my edits");
    assert_eq!(fs::read(a.dir.join("stranger.xmp")).unwrap(), STRANGER);
}

fn organized(a: &Archive, src: &Path, dst: &Path) -> pc_organize::Move {
    let mtime = pc_core::time::mtime_unix(&fs::metadata(src).unwrap());
    let size = fs::metadata(src).unwrap().len() as i64;
    let file_id =
        a.db.upsert_file(
            &pc_db::NewFile {
                path: src.display().to_string(),
                name: src.file_name().unwrap().to_string_lossy().into_owned(),
                size,
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
        size,
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

#[test]
fn organize_never_replaces_what_appears_at_the_destination() {
    for what in strangers() {
        let a = archive();
        let src = a.dir.join("a.jpg");
        let dst = a.dir.join("2019/a.jpg");
        fs::write(&src, b"picture").unwrap();
        let m = organized(&a, &src, &dst);

        let _race = race_at(dst.clone(), what);
        let report = crate::organize(&a.db, a.run, &[m]).unwrap();

        assert_eq!(report.done.frames, 0, "перенос поверх чужого файла");
        let (who, why) = &report.refused[0];
        assert_eq!(who, &src.display().to_string());
        assert!(why.contains(&dst.display().to_string()), "{why}");
        assert_eq!(fs::read(&src).unwrap(), b"picture");
        intact(&dst, what);
        let (status, note) = journal_row(&a.db, last_journal_id(&a.db));
        assert_eq!(status, JournalStatus::Failed.as_str());
        assert!(note.contains(&dst.display().to_string()), "{note}");
    }
}

#[test]
fn an_organize_undo_never_replaces_what_appears_at_the_old_place() {
    let a = archive();
    let src = a.dir.join("a.jpg");
    let dst = a.dir.join("2019/a.jpg");
    fs::write(&src, b"picture").unwrap();
    let m = organized(&a, &src, &dst);
    assert_eq!(crate::organize(&a.db, a.run, &[m]).unwrap().done.frames, 1);

    let _race = race_at(src.clone(), Stranger::File);
    let (back, failed) = crate::undo_run(&a.db, a.run).unwrap();

    assert_eq!(back.entries_back, 0);
    assert!(
        failed
            .iter()
            .any(|f| f.contains(&src.display().to_string())),
        "{failed:?}"
    );
    intact(&src, Stranger::File);
    assert_eq!(fs::read(&dst).unwrap(), b"picture");
}

/// What exFAT answers on macOS, and what Linux answers (`EINVAL`) on a file
/// system without `RENAME_NOREPLACE`: the call is refused and nothing moves.
fn no_exclusive_rename() -> race::Guard {
    race::before_move(|_, _| Err(io::Error::from(io::ErrorKind::Unsupported)))
}

fn journal_rows(db: &Db) -> i64 {
    db.conn
        .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_volume_that_cannot_refuse_to_replace_stops_apply_before_anything_moves() {
    let a = archive();
    let (one, two) = (a.dir.join("one.bmp"), a.dir.join("two.bmp"));
    fs::write(&one, b"first").unwrap();
    fs::write(&two, b"second").unwrap();
    let cs = [candidate(&a.db, a.run, &one), candidate(&a.db, a.run, &two)];

    let _refused = no_exclusive_rename();
    let err = crate::apply(&a.db, a.run, &cs, None).unwrap_err();

    assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
    let shown = err.to_string();
    assert!(
        shown.contains(&a.quarantine.join("one.bmp").display().to_string()),
        "{shown}"
    );
    assert_eq!(fs::read(&one).unwrap(), b"first");
    assert_eq!(fs::read(&two).unwrap(), b"second");
    // One refusal, written down; the second photograph was never tried.
    assert_eq!(journal_rows(&a.db), 1);
    let (status, note) = journal_row(&a.db, last_journal_id(&a.db));
    assert_eq!(status, JournalStatus::Failed.as_str());
    assert!(
        note.contains("RENAME") || note.contains("заменить") || note.contains("replac"),
        "{note}"
    );
}

#[test]
fn a_volume_that_cannot_refuse_to_replace_stops_organize_and_undo() {
    let a = archive();
    let src = a.dir.join("a.jpg");
    let dst = a.dir.join("2019/a.jpg");
    fs::write(&src, b"picture").unwrap();
    let m = organized(&a, &src, &dst);
    {
        let _refused = no_exclusive_rename();
        let err = crate::organize(&a.db, a.run, std::slice::from_ref(&m)).unwrap_err();
        assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
        assert_eq!(fs::read(&src).unwrap(), b"picture");
        assert!(fs::symlink_metadata(&dst).is_err());
    }
    // Moved for real, then the volume stops cooperating on the way back.
    assert_eq!(crate::organize(&a.db, a.run, &[m]).unwrap().done.frames, 1);
    let entry =
        a.db.journal_by_run_op(a.run, "organize")
            .unwrap()
            .pop()
            .unwrap();
    let _refused = no_exclusive_rename();
    let err = crate::undo(&a.db, entry.id).unwrap_err();
    assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
    assert_eq!(fs::read(&dst).unwrap(), b"picture");
    let (status, note) = journal_row(&a.db, entry.id);
    assert_eq!(
        status,
        JournalStatus::Done.as_str(),
        "откат закрыл дверь назад"
    );
    assert!(note.contains(&dst.display().to_string()), "{note}");
}

/// Natively, on a real exFAT volume (macOS disk image): the volume reports
/// it cannot rename without replacing, so apply, organize and an undo are
/// refused before the first move and nothing at all is written there — not
/// even the quarantine folder.
#[cfg(target_os = "macos")]
mod exfat {
    use super::*;
    use std::collections::BTreeMap;
    use std::process::Command;
    use std::sync::Mutex;

    static HDIUTIL: Mutex<()> = Mutex::new(());

    struct Image {
        _dir: tempfile::TempDir,
        mount: PathBuf,
    }

    impl Image {
        fn new(fs_name: &str) -> Self {
            let _one = HDIUTIL.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let image = dir.path().join("volume.dmg");
            let mount = dir.path().join("mnt");
            run(Command::new("hdiutil")
                .args(["create", "-quiet", "-size", "64m", "-fs", fs_name])
                .args(["-volname", "PCTEST"])
                .arg(&image));
            run(Command::new("hdiutil")
                .args(["attach", "-quiet", "-nobrowse", "-noverify", "-mountpoint"])
                .arg(&mount)
                .arg(&image));
            let mount = mount.canonicalize().unwrap();
            Self { _dir: dir, mount }
        }
    }

    impl Drop for Image {
        fn drop(&mut self) {
            let _one = HDIUTIL.lock().unwrap_or_else(|e| e.into_inner());
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.mount)
                .status();
        }
    }

    fn run(command: &mut Command) {
        let out = command.output().unwrap();
        assert!(out.status.success(), "{command:?}: {out:?}");
    }

    fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut all = BTreeMap::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for entry in fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if fs::symlink_metadata(&path).unwrap().is_dir() {
                    todo.push(path.clone());
                    all.insert(path, None);
                } else {
                    all.insert(path.clone(), Some(fs::read(&path).unwrap()));
                }
            }
        }
        all
    }

    #[test]
    fn exfat_is_refused_before_the_first_move() {
        let image = Image::new("ExFAT");
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let dir = image.mount.join("archive");
        fs::create_dir_all(&dir).unwrap();
        let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
        let photo = dir.join("photo.bmp");
        fs::write(&photo, b"synthetic").unwrap();
        fs::write(dir.join("photo.xmp"), b"synthetic edits").unwrap();
        let before = tree(&image.mount);

        let c = candidate(&db, run, &photo);
        let err = crate::apply(&db, run, &[c], None).unwrap_err();
        assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
        assert!(err.to_string().contains("RENAME_EXCL"), "{err}");

        let a = Archive {
            _tmp: tempfile::tempdir().unwrap(),
            dir: dir.clone(),
            quarantine: dir.join(pc_core::QUARANTINE_DIR),
            db,
            run,
        };
        let m = organized(&a, &photo, &dir.join("2019/photo.bmp"));
        let err = crate::organize(&a.db, a.run, &[m]).unwrap_err();
        assert!(crate::is_no_exclusive_rename(&err), "{err:#}");

        assert_eq!(tree(&image.mount), before, "на томе что-то записано");
        assert_eq!(journal_rows(&a.db), 0, "отказ до первого переноса");

        // An undo onto the same volume: a quarantined frame as the journal
        // records it, coming home to exFAT.
        let held = dir.join(pc_core::QUARANTINE_DIR).join("held.bmp");
        fs::create_dir_all(held.parent().unwrap()).unwrap();
        fs::write(&held, b"held").unwrap();
        let home = dir.join("held.bmp");
        let (s, d) = (home.display().to_string(), held.display().to_string());
        let id =
            a.db.journal_begin(&pc_db::NewJournalEntry {
                run_id: a.run,
                op: "quarantine-file",
                target_id: None,
                src: &s,
                dst: Some(&d),
                size: 4,
                file_count: 1,
                manifest: &[pc_db::Moved {
                    src: s.clone(),
                    dst: d.clone(),
                    proof: None,
                }],
            })
            .unwrap();
        a.db.journal_finish(id, JournalStatus::Done, None).unwrap();
        let err = crate::undo(&a.db, id).unwrap_err();
        assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
        assert_eq!(fs::read(&held).unwrap(), b"held");
        assert!(fs::symlink_metadata(&home).is_err());
        assert_eq!(journal_row(&a.db, id).0, JournalStatus::Done.as_str());
    }

    /// A configured quarantine that does not exist yet, on exFAT: refused,
    /// and the volume is exactly as it was — no quarantine folder, no
    /// layout note (el-23goa B1). The default beside-quarantine likewise.
    #[test]
    fn exfat_refusal_with_a_configured_quarantine_writes_nothing() {
        let image = Image::new("ExFAT");
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let dir = image.mount.join("archive");
        fs::create_dir_all(&dir).unwrap();
        let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
        let photo = dir.join("photo.arw");
        fs::write(&photo, b"synthetic frame").unwrap();
        let root = image.mount.join("collected");
        let before = tree(&image.mount);

        for configured in [Some(root.as_path()), None] {
            let c = candidate(&db, run, &photo);
            let err = crate::apply(&db, run, &[c], configured).unwrap_err();
            assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
            assert_eq!(fs::read(&photo).unwrap(), b"synthetic frame");
            assert_eq!(journal_rows(&db), 0);
            assert_eq!(tree(&image.mount), before, "{configured:?}: том изменён");
        }
    }

    /// APFS and HFS+ report the capability; the same apply goes through.
    #[test]
    fn apfs_and_hfs_take_the_move() {
        for fs_name in ["APFS", "HFS+"] {
            let image = Image::new(fs_name);
            let tmp = tempfile::tempdir().unwrap();
            let db = Db::open(&tmp.path().join("test.db")).unwrap();
            let dir = image.mount.join("archive");
            fs::create_dir_all(&dir).unwrap();
            let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
            let photo = dir.join("photo.bmp");
            fs::write(&photo, b"synthetic").unwrap();
            let c = candidate(&db, run, &photo);
            let report = crate::apply(&db, run, &[c], None).unwrap();
            assert_eq!(report.done.frames, 1, "{fs_name}: {:?}", report.refused);
            assert!(dir
                .join(pc_core::QUARANTINE_DIR)
                .join("photo.bmp")
                .is_file());
        }
    }
}

#[path = "refusal_tests.rs"]
mod refusal_tests;

#[path = "round4_tests.rs"]
mod round4_tests;
