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
