//! A unit with an entry that is not a plain file, and an undo whose way
//! home became a link (review el-1nx1u B1–B3, reviewer el-67ku's cases
//! adopted). Shared: they run on Unix with symbolic links and on Windows
//! with junctions and file symbolic links. Disposable fixtures only.

use pc_db::{Db, JournalStatus};
use pc_family::plan::Candidate;
use std::fs;
use std::path::{Path, PathBuf};

struct Fixture {
    tmp: tempfile::TempDir,
    dir: PathBuf,
    photo: PathBuf,
    db: Db,
    run: i64,
}

impl Fixture {
    fn new() -> Self {
        Self::at("")
    }

    /// The photograph in `archive/<below>`, the run's root being `archive`.
    fn at(below: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        let dir = if below.is_empty() {
            root.clone()
        } else {
            root.join(below)
        };
        fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("frame.arw");
        fs::write(&photo, b"synthetic frame 6173").unwrap();
        let db = Db::open(&tmp.path().join("fixture.db")).unwrap();
        let run = db.start_run(&[root.display().to_string()], "test").unwrap();
        Self {
            tmp,
            dir,
            photo,
            db,
            run,
        }
    }

    fn candidate(&self) -> Candidate {
        let size = fs::metadata(&self.photo).unwrap().len() as i64;
        let file_id = self
            .db
            .upsert_file(
                &pc_db::NewFile {
                    path: self.photo.display().to_string(),
                    name: "frame.arw".into(),
                    size,
                    ..Default::default()
                },
                self.run,
            )
            .unwrap();
        Candidate {
            file_id,
            family_id: 0,
            path: self.photo.display().to_string(),
            size,
            role: pc_family::Role::Copy,
            keeper_id: 0,
            keeper_path: String::new(),
            reason: "explicit synthetic selection".into(),
            manual: true,
            group_keeper: String::new(),
        }
    }

    fn rows(&self) -> i64 {
        self.db
            .conn
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap()
    }
}

/// An orphan in quarantine that is a link — to a folder (a junction on
/// Windows), to a file, or to nothing — or a folder, is not carried out:
/// only plain files come out of quarantine (el-4hyy6, the unit rule of
/// el-1nx1u B1). Nothing moves, nothing is written, and what a link leads
/// to is untouched; a plain orphan beside it still comes home.
#[test]
fn an_orphan_that_is_not_a_plain_file_stays_in_quarantine() {
    let f = Fixture::new();
    let q = f.dir.join(pc_core::QUARANTINE_DIR);
    fs::create_dir_all(&q).unwrap();
    let elsewhere = f.tmp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("inside.jpg"), b"the user's 21").unwrap();
    let to_folder = q.join("Album");
    directory_link(&to_folder, &elsewhere);
    let to_file = q.join("linked.jpg");
    file_link(&to_file, &elsewhere.join("inside.jpg"));
    let dangling = q.join("gone");
    directory_link(&dangling, &f.tmp.path().join("nowhere"));
    let folder = q.join("Folder");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("x.jpg"), b"inside 9").unwrap();

    for at in [&to_folder, &to_file, &dangling, &folder] {
        let home = f.dir.join(at.file_name().unwrap());
        let r = crate::adopt_orphan(
            &f.db,
            f.run,
            &at.display().to_string(),
            &home.display().to_string(),
        );
        let e = r.expect_err(&format!("{} came out", at.display()));
        assert!(
            format!("{e:#}").contains(&at.display().to_string()),
            "{e:#}"
        );
        assert!(fs::symlink_metadata(at).is_ok(), "{}", at.display());
        assert!(fs::symlink_metadata(&home).is_err(), "{}", home.display());
    }
    assert_eq!(f.rows(), 0);
    assert_eq!(
        fs::read(elsewhere.join("inside.jpg")).unwrap(),
        b"the user's 21"
    );
    assert_eq!(fs::read(folder.join("x.jpg")).unwrap(), b"inside 9");

    let plain = q.join("plain.jpg");
    fs::write(&plain, b"orphan 8").unwrap();
    let home = f.dir.join("plain.jpg");
    crate::adopt_orphan(
        &f.db,
        f.run,
        &plain.display().to_string(),
        &home.display().to_string(),
    )
    .unwrap();
    assert_eq!(fs::read(&home).unwrap(), b"orphan 8");
}

/// A link to a folder: a symbolic link on Unix, a junction on Windows (no
/// privilege needed).
fn directory_link(at: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, at).unwrap();
    #[cfg(windows)]
    {
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(at)
            .arg(target)
            .output()
            .unwrap();
        assert!(out.status.success(), "mklink /J: {out:?}");
    }
}

/// A file symbolic link. On Windows it needs the privilege the CI runner
/// and the test VM have (elevated): the test fails loudly without it.
fn file_link(at: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, at).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, at)
        .expect("a file symbolic link needs an elevated runner or Developer Mode");
}

fn remove_link(at: &Path) {
    #[cfg(unix)]
    fs::remove_file(at).unwrap();
    // A junction is removed as a folder; what it leads to is not touched.
    #[cfg(windows)]
    fs::remove_dir(at).unwrap();
}

/// Bytes, identity, permissions and times. Reading may update the access
/// time, so it is left out.
fn snapshot(path: &Path) -> (Vec<u8>, String) {
    let f = fs::File::open(path).unwrap();
    let md = f.metadata().unwrap();
    let proof = pc_core::proof::Proof::of_file(&f);
    #[allow(unused_mut)]
    let mut details = format!(
        "{proof:?}/{:?}/{:?}/{:?}",
        md.permissions(),
        md.modified(),
        md.created().ok()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        details.push_str(&format!(
            "/{}/{}/{}/{}",
            md.uid(),
            md.gid(),
            md.mode(),
            md.nlink()
        ));
    }
    (fs::read(path).unwrap(), details)
}

/// The refusal of a unit for an entry under a companion's name: nothing
/// moved, the frame at home, nothing journaled, and the words the web's
/// preview shows in advance are the words the run gives.
fn assert_unit_refused(f: &Fixture, side: &Path) {
    let photo_before = snapshot(&f.photo);
    let doubt = crate::files::companion_doubt(&f.photo).expect("the preview says it in advance");
    let report = crate::apply(&f.db, f.run, &[f.candidate()], None).unwrap();
    assert_eq!(report.done.frames, 0, "{report:?}");
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert_eq!(report.refused[0].1, doubt);
    assert!(doubt.contains(&side.display().to_string()), "{doubt}");
    assert_eq!(snapshot(&f.photo), photo_before);
    assert!(fs::symlink_metadata(side).is_ok(), "the entry is gone");
    assert_eq!(f.rows(), 0);
    assert!(!f
        .dir
        .join(pc_core::QUARANTINE_DIR)
        .join("frame.arw")
        .exists());
}

#[test]
fn a_link_to_a_folder_under_a_sidecar_name_refuses_the_frame() {
    let f = Fixture::new();
    let foreign = f.tmp.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    let payload = foreign.join("payload");
    fs::write(&payload, b"foreign edits 731").unwrap();
    let before = snapshot(&payload);
    let side = f.dir.join("frame.xmp");
    directory_link(&side, &foreign);

    assert_unit_refused(&f, &side);
    assert_eq!(snapshot(&payload), before, "the foreign folder was touched");
    assert_eq!(fs::read_dir(&foreign).unwrap().count(), 1);
}

#[test]
fn a_dangling_link_under_a_sidecar_name_refuses_the_frame() {
    let f = Fixture::new();
    let side = f.dir.join("frame.xmp");
    file_link(&side, &f.tmp.path().join("absent-edits"));

    assert_unit_refused(&f, &side);
}

/// A sidecar that is a link to a real file is neither followed nor moved
/// as itself (director decision on el-1nx1u, criterion 4): the edits it
/// leads to stay untouched, and so does the frame.
#[test]
fn a_link_to_a_file_under_a_sidecar_name_refuses_the_frame() {
    let f = Fixture::new();
    let target = f.tmp.path().join("foreign-edits.xmp");
    fs::write(&target, b"foreign metadata 8172").unwrap();
    let before = snapshot(&target);
    let side = f.dir.join("frame.xmp");
    file_link(&side, &target);

    assert_unit_refused(&f, &side);
    assert_eq!(snapshot(&target), before);
}

#[test]
fn a_folder_under_a_sidecar_name_refuses_the_frame() {
    let f = Fixture::new();
    let side = f.dir.join("frame.arw.xmp");
    fs::create_dir(&side).unwrap();
    fs::write(side.join("inside"), b"not a sidecar 412").unwrap();

    assert_unit_refused(&f, &side);
    assert_eq!(fs::read(side.join("inside")).unwrap(), b"not a sidecar 412");
}

/// A frame and a plain sidecar still go together: the rule refuses only
/// what is not a plain file.
#[test]
fn a_plain_sidecar_still_travels_with_its_frame() {
    let f = Fixture::new();
    fs::write(f.dir.join("frame.xmp"), b"synthetic edits 291").unwrap();
    assert_eq!(crate::files::companion_doubt(&f.photo), None);
    let report = crate::apply(&f.db, f.run, &[f.candidate()], None).unwrap();
    assert_eq!(
        (report.done.frames, report.done.companions),
        (1, 1),
        "{report:?}"
    );
}

/// The way home replaced by a link after a finished move (B3): the folder
/// the frame and its sidecar left was renamed aside and a link to a foreign
/// folder put in its place. Undo refuses, names the place, moves nothing
/// into the foreign folder, keeps both members in quarantine and the entry
/// done; once the folder is back, the same entry is undone.
fn undo_through_a_replaced_folder(below: &str, replace: &dyn Fn(&Fixture) -> PathBuf) {
    let f = Fixture::at(below);
    fs::write(f.dir.join("frame.xmp"), b"synthetic edits 291").unwrap();
    let gathered = f.tmp.path().join("gathered");
    let report = crate::apply(&f.db, f.run, &[f.candidate()], Some(&gathered)).unwrap();
    assert_eq!(
        (report.done.frames, report.done.companions),
        (1, 1),
        "{report:?}"
    );
    let entry = f.db.journal_quarantined(None).unwrap().pop().unwrap();

    let replaced = replace(&f);
    let aside = replaced.with_file_name("aside");
    fs::rename(&replaced, &aside).unwrap();
    let foreign = f.tmp.path().join("foreign");
    fs::create_dir_all(foreign.join(below)).unwrap();
    let sentinel = foreign.join("sentinel");
    fs::write(&sentinel, b"foreign namespace 871").unwrap();
    let before = snapshot(&sentinel);
    directory_link(&replaced, &foreign);

    let err = crate::undo(&f.db, entry.id).unwrap_err();
    let shown = format!("{err:#}");
    assert!(
        shown.contains(&replaced.display().to_string()),
        "the unsafe place is not named: {shown}"
    );
    assert_eq!(snapshot(&sentinel), before);
    let landed = foreign.join(below);
    assert!(!landed.join("frame.arw").exists(), "{shown}");
    assert!(!landed.join("frame.xmp").exists(), "{shown}");
    let now = f.db.journal_entry(entry.id).unwrap().unwrap();
    assert_eq!(now.status, JournalStatus::Done);
    for m in &entry.manifest {
        assert!(Path::new(&m.dst).exists(), "{} left quarantine", m.dst);
    }

    remove_link(&replaced);
    fs::rename(&aside, &replaced).unwrap();
    let done = crate::undo(&f.db, entry.id).unwrap();
    assert_eq!(done.files_back, 2);
    assert_eq!(fs::read(&f.photo).unwrap(), b"synthetic frame 6173");
    assert_eq!(
        fs::read(f.dir.join("frame.xmp")).unwrap(),
        b"synthetic edits 291"
    );
}

#[test]
fn undo_refuses_a_home_folder_replaced_by_a_link() {
    undo_through_a_replaced_folder("", &|f| f.dir.clone());
}

/// The run's root replaced, the frame in a folder below it: the folder
/// below exists in the foreign tree too, and is still not home.
#[test]
fn undo_refuses_a_run_root_replaced_by_a_link_above_the_home_folder() {
    undo_through_a_replaced_folder("2024", &|f| f.dir.parent().unwrap().to_path_buf());
}

#[test]
fn undo_collision_preserves_foreign_bytes_metadata_and_the_finished_record() {
    let f = Fixture::new();
    let side = f.dir.join("frame.xmp");
    fs::write(&side, b"synthetic edits 291").unwrap();
    crate::apply(&f.db, f.run, &[f.candidate()], None).unwrap();
    let entry = f.db.journal_quarantined(None).unwrap().pop().unwrap();
    fs::write(&side, b"foreign sidecar 9871").unwrap();
    let before = snapshot(&side);

    assert!(crate::undo(&f.db, entry.id).is_err());
    assert_eq!(snapshot(&side), before);
    assert!(!f.photo.exists());
    assert_eq!(
        f.db.journal_entry(entry.id).unwrap().unwrap().status,
        JournalStatus::Done
    );
    for m in &entry.manifest {
        assert!(Path::new(&m.dst).exists());
    }
}

#[test]
fn a_second_member_failure_refuses_and_puts_the_frame_back() {
    let f = Fixture::new();
    let side = f.dir.join("frame.xmp");
    fs::write(&side, b"synthetic edits 291").unwrap();
    let photo_before = snapshot(&f.photo);
    let side_before = snapshot(&side);
    let hook_side = side.clone();
    let _guard =
        crate::race::before_move_under(&f.tmp.path().display().to_string(), move |src, _| {
            if src == hook_side {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected sidecar failure",
                ));
            }
            Ok(())
        });

    let report = crate::apply(&f.db, f.run, &[f.candidate()], None).unwrap();

    assert_eq!(report.done.frames, 0);
    assert_eq!(report.refused.len(), 1);
    assert_eq!(snapshot(&f.photo), photo_before);
    assert_eq!(snapshot(&side), side_before);
    assert!(f.db.journal_quarantined(None).unwrap().is_empty());
}

#[test]
fn a_blocked_compensating_return_keeps_and_names_every_member() {
    let f = Fixture::new();
    let side = f.dir.join("frame.xmp");
    fs::write(&side, b"synthetic edits 291").unwrap();
    let side_before = snapshot(&side);
    let home = f.photo.clone();
    let hook_side = side.clone();
    let foreign_before = std::sync::Arc::new(std::sync::Mutex::new(None));
    let saved = foreign_before.clone();
    let _guard =
        crate::race::before_move_under(&f.tmp.path().display().to_string(), move |src, _| {
            if src == hook_side {
                fs::write(&home, b"foreign replacement 9101").unwrap();
                *saved.lock().unwrap() = Some(snapshot(&home));
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected sidecar failure",
                ));
            }
            Ok(())
        });

    let err = crate::apply(&f.db, f.run, &[f.candidate()], None).unwrap_err();

    let shown = format!("{err:#}");
    assert!(crate::stopped_run(&err).is_some(), "no typed stop: {shown}");
    assert_eq!(
        snapshot(&f.photo),
        foreign_before.lock().unwrap().clone().unwrap()
    );
    assert_eq!(snapshot(&side), side_before);
    let quarantined = f.dir.join(pc_core::QUARANTINE_DIR).join("frame.arw");
    assert_eq!(fs::read(&quarantined).unwrap(), b"synthetic frame 6173");
    assert!(
        shown.contains(&quarantined.display().to_string()),
        "{shown}"
    );
    assert!(shown.contains(&side.display().to_string()), "{shown}");
    let entry = f.db.journal_entry(1).unwrap().unwrap();
    assert_eq!(entry.status, JournalStatus::Pending);
    assert_eq!(entry.manifest.len(), 2);
    assert!(!entry.located.is_empty());
}

/// The row an in-use refusal leaves (P3a; the refusal itself only happens
/// on Windows, `windows_tests` proves it there): failed, with the reason,
/// and claiming nothing — no destination, no list, no size — so that
/// neither undo nor purge ever acts on it, and the photograph stays home.
#[test]
fn an_in_use_refusal_is_written_down_and_claims_nothing() {
    let f = Fixture::new();
    let c = f.candidate();
    let why = crate::in_use_text(&f.photo);
    let before = snapshot(&f.photo);

    let shown = crate::files::record_in_use(&f.db, f.run, &c, why.clone());

    assert_eq!(shown, why);
    let entry = f.db.journal_entry(1).unwrap().expect("the refusal's row");
    assert_eq!(entry.status, JournalStatus::Failed);
    assert_eq!(entry.target_id, Some(c.file_id));
    assert_eq!(entry.dst, None);
    assert!(entry.manifest.is_empty());
    assert_eq!((entry.size, entry.file_count), (0, 0));
    let events = f.db.journal_events(1).unwrap();
    assert!(events.iter().any(|e| e.text == why), "{events:?}");
    assert!(crate::undo(&f.db, 1).is_err());
    assert!(f.db.journal_quarantined(None).unwrap().is_empty());
    assert_eq!(snapshot(&f.photo), before);
}

// ---- the folder the files left, by identity (director decision after
// el-4r8yf B3-R2) -----------------------------------------------------------

/// A frame and its sidecar in `archive/2024`, quarantined into a gathered
/// quarantine outside the archive; the finished entry.
fn quarantined_below(f: &Fixture) -> pc_db::JournalEntry {
    fs::write(f.dir.join("frame.xmp"), b"synthetic edits 291").unwrap();
    let gathered = f.tmp.path().join("gathered");
    let report = crate::apply(&f.db, f.run, &[f.candidate()], Some(&gathered)).unwrap();
    assert_eq!(
        (report.done.frames, report.done.companions),
        (1, 1),
        "{report:?}"
    );
    f.db.journal_quarantined(None).unwrap().pop().unwrap()
}

/// Both members still in quarantine, the entry still done and undoable,
/// nothing in `place`.
fn assert_kept(f: &Fixture, entry: &pc_db::JournalEntry, place: &Path) {
    let now = f.db.journal_entry(entry.id).unwrap().unwrap();
    assert_eq!(now.status, JournalStatus::Done);
    assert!(crate::undo_offered(&now));
    for m in &entry.manifest {
        assert!(Path::new(&m.dst).exists(), "{} left quarantine", m.dst);
    }
    assert!(!place.join("frame.arw").exists());
    assert!(!place.join("frame.xmp").exists());
}

fn later_nested_scan(f: &Fixture) {
    f.db.start_run(&[f.dir.display().to_string()], "later ordinary scan")
        .unwrap();
}

/// Each member records the folder it left — the folder held for its move.
#[test]
fn a_move_records_the_folder_each_member_left() {
    let f = Fixture::at("2024");
    let entry = quarantined_below(&f);
    let held = pc_core::anchored::Dir::open(&f.dir).unwrap();
    let id = pc_db::FolderId::of(held.ident().unwrap());
    assert_eq!(entry.manifest.len(), 2);
    for m in &entry.manifest {
        assert_eq!(m.home, Some(id), "{m:?}");
    }
}

/// el-4r8yf B3-R2: a later scan of the folder the files left, then the
/// original root replaced by a link (a junction on Windows) to a look-alike
/// tree. The undo refuses, names the folder, sends nothing into the foreign
/// tree, and both members stay in quarantine with the entry done.
#[test]
fn a_later_nested_scan_does_not_let_a_replaced_ancestor_link_redirect_the_undo() {
    let f = Fixture::at("2024");
    let entry = quarantined_below(&f);
    later_nested_scan(&f);
    let root = f.dir.parent().unwrap().to_path_buf();
    let aside = f.tmp.path().join("old-archive");
    fs::rename(&root, &aside).unwrap();
    let foreign = f.tmp.path().join("foreign");
    fs::create_dir_all(foreign.join("2024")).unwrap();
    let sentinel = foreign.join("2024/sentinel");
    fs::write(&sentinel, b"foreign namespace 8173").unwrap();
    let before = snapshot(&sentinel);
    directory_link(&root, &foreign);

    let err = crate::undo(&f.db, entry.id).unwrap_err();
    let shown = format!("{err:#}");
    assert!(shown.contains("is not the one the files left"), "{shown}");
    assert!(shown.contains(&f.dir.display().to_string()), "{shown}");
    assert_eq!(snapshot(&sentinel), before);
    assert_eq!(fs::read_dir(foreign.join("2024")).unwrap().count(), 1);
    assert_kept(&f, &entry, &foreign.join("2024"));

    // The archive back in its place: the same entry comes home.
    remove_link(&root);
    fs::rename(&aside, &root).unwrap();
    let done = crate::undo(&f.db, entry.id).unwrap();
    assert_eq!(done.files_back, 2);
    assert_eq!(fs::read(&f.photo).unwrap(), b"synthetic frame 6173");
}

/// The control of the pair above: the same later scan, no link. The undo
/// brings both members home and closes the entry.
#[test]
fn a_later_nested_scan_alone_does_not_stop_an_ordinary_undo() {
    let f = Fixture::at("2024");
    let entry = quarantined_below(&f);
    later_nested_scan(&f);

    let done = crate::undo(&f.db, entry.id).unwrap();
    assert_eq!((done.entries_back, done.files_back), (1, 2));
    assert_eq!(fs::read(&f.photo).unwrap(), b"synthetic frame 6173");
    assert_eq!(
        fs::read(f.dir.join("frame.xmp")).unwrap(),
        b"synthetic edits 291"
    );
    assert_eq!(
        f.db.journal_entry(entry.id).unwrap().unwrap().status,
        JournalStatus::Undone
    );
    for m in &entry.manifest {
        assert!(!Path::new(&m.dst).exists(), "{} still in quarantine", m.dst);
    }
}

/// The folder renamed aside: missing at its path, the undo refuses; made
/// again, empty, at the same path, it is another folder and the undo still
/// refuses — a path that looks right is not the folder the files left.
/// The folder back in place, the entry is undone.
#[test]
fn a_folder_renamed_aside_and_made_again_is_not_home() {
    let f = Fixture::at("2024");
    let entry = quarantined_below(&f);
    let aside = f.dir.with_file_name("2024-aside");
    fs::rename(&f.dir, &aside).unwrap();

    let err = crate::undo(&f.db, entry.id).unwrap_err();
    let shown = format!("{err:#}");
    assert!(shown.contains("is not the one the files left"), "{shown}");
    assert!(shown.contains(&f.dir.display().to_string()), "{shown}");
    assert!(!f.dir.exists(), "the undo made the folder: {shown}");
    assert_kept(&f, &entry, &aside);

    fs::create_dir(&f.dir).unwrap();
    let err = crate::undo(&f.db, entry.id).unwrap_err();
    let shown = format!("{err:#}");
    assert!(shown.contains("is not the one the files left"), "{shown}");
    assert_eq!(fs::read_dir(&f.dir).unwrap().count(), 0, "{shown}");
    assert_kept(&f, &entry, &aside);

    fs::remove_dir(&f.dir).unwrap();
    fs::rename(&aside, &f.dir).unwrap();
    let done = crate::undo(&f.db, entry.id).unwrap();
    assert_eq!(done.files_back, 2);
    assert_eq!(fs::read(&f.photo).unwrap(), b"synthetic frame 6173");
    assert_eq!(
        f.db.journal_entry(entry.id).unwrap().unwrap().status,
        JournalStatus::Undone
    );
}

/// A row written before folder identities were recorded: the frame and its
/// sidecar from `archive/2024`, quarantined in `gathered`, by a run rooted
/// at `archive`.
fn legacy_row(f: &Fixture) -> i64 {
    let held = f.tmp.path().join("gathered");
    fs::create_dir_all(&held).unwrap();
    let side = f.dir.join("frame.xmp");
    fs::write(&side, b"synthetic edits 291").unwrap();
    fs::rename(&f.photo, held.join("frame.arw")).unwrap();
    fs::rename(&side, held.join("frame.xmp")).unwrap();
    let manifest: Vec<pc_db::Moved> = ["frame.arw", "frame.xmp"]
        .iter()
        .map(|n| pc_db::Moved {
            src: f.dir.join(n).display().to_string(),
            dst: held.join(n).display().to_string(),
            proof: crate::files::evidence(&held.join(n)).unwrap(),
            home: None,
        })
        .collect();
    let id =
        f.db.journal_begin(&pc_db::NewJournalEntry {
            run_id: f.run,
            op: "quarantine-file",
            target_id: None,
            src: &manifest[0].src,
            dst: Some(&manifest[0].dst),
            size: 39,
            file_count: 2,
            manifest: &manifest,
        })
        .unwrap();
    f.db.journal_finish(id, JournalStatus::Done, None).unwrap();
    id
}

/// An old row is checked up to its own run's root and no other: another
/// run's root deep inside does not shorten the check, and the replaced
/// root is refused.
#[test]
fn an_old_row_is_bounded_by_its_own_run_root_not_by_another_runs() {
    let f = Fixture::at("2024");
    let id = legacy_row(&f);
    later_nested_scan(&f);
    let root = f.dir.parent().unwrap().to_path_buf();
    let aside = f.tmp.path().join("old-archive");
    fs::rename(&root, &aside).unwrap();
    let foreign = f.tmp.path().join("foreign");
    fs::create_dir_all(foreign.join("2024")).unwrap();
    directory_link(&root, &foreign);

    let err = crate::undo(&f.db, id).unwrap_err();
    let shown = format!("{err:#}");
    assert!(shown.contains("a symbolic link or a junction"), "{shown}");
    assert!(shown.contains(&root.display().to_string()), "{shown}");
    assert_eq!(fs::read_dir(foreign.join("2024")).unwrap().count(), 0);
    let entry = f.db.journal_entry(id).unwrap().unwrap();
    assert_kept(&f, &entry, &foreign.join("2024"));

    // The root back: the old row is still undone by its path.
    remove_link(&root);
    fs::rename(&aside, &root).unwrap();
    let done = crate::undo(&f.db, id).unwrap();
    assert_eq!(done.files_back, 2);
    assert_eq!(fs::read(&f.photo).unwrap(), b"synthetic frame 6173");
}

/// An old row whose files' folder is under none of its own run's roots —
/// only under another run's — has no bound, and is refused.
#[test]
fn an_old_row_outside_its_own_run_roots_is_refused() {
    let f = Fixture::at("2024");
    let id = legacy_row(&f);
    // The entry's run is re-pointed at a root that does not hold the files:
    // a fixture of a run rooted elsewhere.
    f.db.conn
        .execute(
            "UPDATE runs SET roots = ?1 WHERE id = ?2",
            rusqlite::params![
                format!(
                    "[{:?}]",
                    f.tmp.path().join("elsewhere").display().to_string()
                ),
                f.run
            ],
        )
        .unwrap();
    later_nested_scan(&f);

    let err = crate::undo(&f.db, id).unwrap_err();
    let shown = format!("{err:#}");
    assert!(
        shown.contains("is under none of the folders this entry's run scanned"),
        "{shown}"
    );
    let entry = f.db.journal_entry(id).unwrap().unwrap();
    assert_kept(&f, &entry, &f.dir);
}

/// A planned organize move for the fixture's photograph into `new/`.
fn organize_move(f: &Fixture, size: i64, mtime: i64) -> pc_organize::Move {
    pc_organize::Move {
        file_id: 0,
        src: f.photo.display().to_string(),
        dst: f.tmp.path().join("new/frame.arw").display().to_string(),
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

/// Organize looks at the frame as itself before anything follows it
/// (el-4hyy6 B1): a file link, a link to a folder (a junction on Windows)
/// or a folder standing at the planned frame's name refuses the whole unit
/// before the journal is written — the link stays, its plain sidecar stays,
/// nothing is counted as done, and what it leads to is untouched.
#[test]
fn organize_refuses_a_frame_that_is_not_a_plain_file_before_any_move() {
    for case in ["file link", "folder link", "folder"] {
        let f = Fixture::new();
        let md = fs::metadata(&f.photo).unwrap();
        let m = organize_move(&f, md.len() as i64, pc_core::time::mtime_unix(&md));
        let side = f.dir.join("frame.xmp");
        fs::write(&side, b"synthetic edits 291").unwrap();
        let aside = f.tmp.path().join("saved-frame.arw");
        fs::rename(&f.photo, &aside).unwrap();
        let foreign = f.tmp.path().join("foreign");
        fs::create_dir_all(&foreign).unwrap();
        let target = foreign.join("frame.arw");
        fs::copy(&aside, &target).unwrap();
        match case {
            "file link" => file_link(&f.photo, &target),
            "folder link" => directory_link(&f.photo, &foreign),
            _ => fs::create_dir(&f.photo).unwrap(),
        }
        let before = snapshot(&target);

        let r = crate::organize(&f.db, f.run, std::slice::from_ref(&m)).unwrap();
        assert_eq!(r.done.frames, 0, "{case}: {r:?}");
        assert_eq!(r.done.companions, 0, "{case}: {r:?}");
        let why = &r.refused.first().expect(case).1;
        assert!(why.contains(&m.src), "{case}: {why}");
        assert!(why.contains("not a plain file"), "{case}: {why}");
        assert_eq!(f.rows(), 0, "{case}: a refused frame writes no row");
        assert!(fs::symlink_metadata(&f.photo).is_ok(), "{case}");
        assert_eq!(fs::read(&side).unwrap(), b"synthetic edits 291", "{case}");
        assert!(fs::symlink_metadata(&m.dst).is_err(), "{case}");
        assert!(
            fs::symlink_metadata(f.tmp.path().join("new/frame.xmp")).is_err(),
            "{case}"
        );
        assert_eq!(snapshot(&target), before, "{case}");
        assert_eq!(fs::read_dir(&foreign).unwrap().count(), 1, "{case}");
    }
}

/// An undo whose row recorded a link as its frame — no row this version
/// writes, but one an older candidate could have — moves nothing: only
/// plain files come back, the row stays open, and the link and what it
/// leads to are where they were.
#[test]
fn an_undo_does_not_bring_back_a_frame_recorded_as_a_link() {
    let f = Fixture::new();
    let new = f.tmp.path().join("new");
    fs::create_dir_all(&new).unwrap();
    let target = f.tmp.path().join("foreign.arw");
    fs::rename(&f.photo, &target).unwrap();
    let arrived = new.join("frame.arw");
    file_link(&arrived, &target);
    let before = snapshot(&target);
    let manifest = [pc_db::Moved {
        src: f.photo.display().to_string(),
        dst: arrived.display().to_string(),
        proof: crate::files::evidence(&arrived).unwrap(),
        home: Some(crate::bound::folder_of(&f.photo).unwrap()),
    }];
    assert!(!crate::files::plain_proof(manifest[0].proof.as_ref()));
    let id =
        f.db.journal_begin(&pc_db::NewJournalEntry {
            run_id: f.run,
            op: "organize",
            target_id: None,
            src: &manifest[0].src,
            dst: Some(&manifest[0].dst),
            size: 0,
            file_count: 1,
            manifest: &manifest,
        })
        .unwrap();
    f.db.journal_finish(id, JournalStatus::Done, None).unwrap();

    let err = crate::undo(&f.db, id).unwrap_err();
    let shown = format!("{err:#}");
    assert!(shown.contains("only plain files come back"), "{shown}");
    assert!(fs::symlink_metadata(&arrived)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(&f.photo).is_err(), "{shown}");
    assert_eq!(snapshot(&target), before);
    let now = f.db.journal_entry(id).unwrap().unwrap();
    assert_eq!(now.status, JournalStatus::Done);
}
