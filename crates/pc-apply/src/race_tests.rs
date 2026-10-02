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
use std::path::{Path, PathBuf};

/// What turns up at the destination.
#[derive(Clone, Copy)]
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

#[cfg(windows)]
fn link(target: &Path, at: &Path) {
    std::os::windows::fs::symlink_file(target, at).unwrap();
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
    for what in [Stranger::File, Stranger::Dangling] {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        fs::write(&photo, b"picture").unwrap();
        let c = candidate(&a.db, a.run, &photo);
        let dst = a.quarantine.join("photo.bmp");

        let _race = race_at(dst.clone(), what);
        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

        assert_eq!(report.totals.files, 0, "перенос поверх чужого файла");
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

    // The photograph moved; its sidecar did not, and says so.
    assert_eq!(report.totals.files, 1);
    let (_, stuck) = &report.refused[0];
    assert!(stuck.contains("frame.xmp"), "{stuck}");
    intact(&taken, Stranger::File);
    assert_eq!(fs::read(a.dir.join("frame.xmp")).unwrap(), b"my edits");
    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();
    assert_eq!(entry.manifest.len(), 1, "спутник записан как уехавший");
}

#[test]
fn a_bundle_never_replaces_what_appears_in_quarantine() {
    let a = archive();
    let previews = a.dir.join("Previews.lrdata");
    fs::create_dir_all(&previews).unwrap();
    fs::write(previews.join("cache"), b"cached").unwrap();
    a.db.upsert_bundle(
        &pc_db::model::NewBundle {
            path: previews.display().to_string(),
            is_dir: true,
            disk: "root".into(),
            dev: 0,
            mount: a.dir.display().to_string(),
            kind: pc_core::DerivedKind::LrPreviews,
            owner_ref: None,
            file_count: 1,
            size: 6,
            newest_mtime: pc_core::time::mtime_unix(&fs::metadata(&previews).unwrap()),
        },
        a.run,
    )
    .unwrap();
    let b =
        a.db.list_bundles(&Default::default())
            .unwrap()
            .pop()
            .unwrap();
    let dst = a.quarantine.join("Previews.lrdata");

    let _race = race_at(dst.clone(), Stranger::EmptyDir);
    let totals = crate::quarantine_many(&a.db, a.run, &[b], None).unwrap();

    assert_eq!(totals.bundles, 0, "каталог уехал поверх чужого");
    assert!(
        totals.skipped[0].contains(&dst.display().to_string()),
        "{:?}",
        totals.skipped
    );
    assert_eq!(fs::read(previews.join("cache")).unwrap(), b"cached");
    intact(&dst, Stranger::EmptyDir);
    let (status, _) = journal_row(&a.db, last_journal_id(&a.db));
    assert_eq!(status, JournalStatus::Failed.as_str());
}

#[test]
fn an_undo_never_replaces_what_appears_at_home() {
    for what in [Stranger::File, Stranger::Dangling] {
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

    assert!(err.contains("frame.xmp"), "{err}");
    assert_eq!(fs::read(&photo).unwrap(), b"raw");
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
    for what in [Stranger::File, Stranger::Dangling] {
        let a = archive();
        let src = a.dir.join("a.jpg");
        let dst = a.dir.join("2019/a.jpg");
        fs::write(&src, b"picture").unwrap();
        let m = organized(&a, &src, &dst);

        let _race = race_at(dst.clone(), what);
        let report = crate::organize(&a.db, a.run, &[m]).unwrap();

        assert_eq!(report.moved, 0, "перенос поверх чужого файла");
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
    assert_eq!(crate::organize(&a.db, a.run, &[m]).unwrap().moved, 1);

    let _race = race_at(src.clone(), Stranger::File);
    let (back, failed) = crate::undo_run(&a.db, a.run).unwrap();

    assert_eq!(back, 0);
    assert!(
        failed
            .iter()
            .any(|f| f.contains(&src.display().to_string())),
        "{failed:?}"
    );
    intact(&src, Stranger::File);
    assert_eq!(fs::read(&dst).unwrap(), b"picture");
}
