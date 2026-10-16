//! Old rows of web runs that recorded no roots (`roots = []`), walked back
//! after el-6s3go (el-53l7h): bounded by the volume's mount point, every
//! component below it held and never followed. Disposable fixtures only.

use pc_db::{Db, JournalStatus, Moved};
use std::fs;
use std::path::{Path, PathBuf};

const FRAME: &[u8] = b"synthetic frame 6173";
const EDITS: &[u8] = b"synthetic edits 291";

struct Fixture {
    _tmp: tempfile::TempDir,
    /// The temporary folder as the system names it without links: on macOS
    /// `/private/var/...`, not `/var/...` (a link below the mount point).
    base: PathBuf,
    archive: PathBuf,
    dir: PathBuf,
    held: PathBuf,
    db: Db,
    run: i64,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let archive = base.join("archive");
        let dir = archive.join("2015").join("Карелия");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("frame.arw"), FRAME).unwrap();
        fs::write(dir.join("frame.xmp"), EDITS).unwrap();
        let held = base.join("gathered");
        fs::create_dir_all(&held).unwrap();
        let db = Db::open(&base.join("fixture.db")).unwrap();
        // An older web run: no roots recorded.
        let run = db.start_run(&[], "0.9 web").unwrap();
        Self {
            _tmp: tmp,
            base,
            archive,
            dir,
            held,
            db,
            run,
        }
    }

    /// The frame and its sidecar moved to quarantine as an earlier version
    /// did, with evidence and without the folder's identity (`home`), or
    /// with it when `identity`.
    fn row(&self, identity: bool, finished: bool) -> i64 {
        let home = identity.then(|| {
            let d = pc_core::anchored::Dir::open(&self.dir).unwrap();
            pc_db::FolderId::of(d.ident().unwrap())
        });
        for n in ["frame.arw", "frame.xmp"] {
            fs::rename(self.dir.join(n), self.held.join(n)).unwrap();
        }
        let manifest: Vec<Moved> = ["frame.arw", "frame.xmp"]
            .iter()
            .map(|n| Moved {
                src: self.dir.join(n).display().to_string(),
                dst: self.held.join(n).display().to_string(),
                proof: crate::files::evidence(&self.held.join(n)).unwrap(),
                home,
            })
            .collect();
        let id = self
            .db
            .journal_begin(&pc_db::NewJournalEntry {
                run_id: self.run,
                op: "quarantine-file",
                target_id: None,
                src: &manifest[0].src,
                dst: Some(&manifest[0].dst),
                size: (FRAME.len() + EDITS.len()) as i64,
                file_count: 2,
                manifest: &manifest,
            })
            .unwrap();
        if finished {
            self.db
                .journal_finish(id, JournalStatus::Done, None)
                .unwrap();
        }
        id
    }

    fn status(&self, id: i64) -> JournalStatus {
        self.db.journal_entry(id).unwrap().unwrap().status
    }

    fn in_quarantine(&self) -> bool {
        fs::read(self.held.join("frame.arw")).unwrap() == FRAME
            && fs::read(self.held.join("frame.xmp")).unwrap() == EDITS
    }

    fn at_home(&self) -> bool {
        fs::read(self.dir.join("frame.arw")).is_ok_and(|b| b == FRAME)
            && fs::read(self.dir.join("frame.xmp")).is_ok_and(|b| b == EDITS)
            && !self.held.join("frame.arw").exists()
            && !self.held.join("frame.xmp").exists()
    }

    /// The archive renamed aside and a link to a look-alike tree in its
    /// place; the look-alike's copy of the folder, empty.
    fn archive_replaced_by_link(&self) -> (PathBuf, PathBuf) {
        let aside = self.base.join("archive-aside");
        fs::rename(&self.archive, &aside).unwrap();
        let foreign = self.base.join("foreign");
        let look_alike = foreign.join("2015").join("Карелия");
        fs::create_dir_all(&look_alike).unwrap();
        std::os::unix::fs::symlink(&foreign, &self.archive).unwrap();
        (aside, look_alike)
    }

    fn archive_back(&self, aside: &Path) {
        fs::remove_file(&self.archive).unwrap();
        fs::rename(aside, &self.archive).unwrap();
    }
}

#[test]
fn an_old_row_of_a_run_without_roots_is_undone_by_its_mount_point() {
    let f = Fixture::new();
    let id = f.row(false, true);

    let done = crate::undo(&f.db, id).unwrap();
    assert_eq!((done.entries_back, done.files_back), (1, 2));
    assert!(f.at_home());
    assert_eq!(f.status(id), JournalStatus::Undone);
}

#[test]
fn an_ancestor_replaced_by_a_link_stops_the_old_row_and_keeps_it_done() {
    let f = Fixture::new();
    let id = f.row(false, true);
    let (aside, look_alike) = f.archive_replaced_by_link();

    let err = crate::undo(&f.db, id).unwrap_err();
    let shown = format!("{err:#}");
    assert!(shown.contains("a symbolic link or a junction"), "{shown}");
    assert!(
        shown.contains(&f.archive.display().to_string()),
        "the refusal names the place: {shown}"
    );
    assert_eq!(fs::read_dir(&look_alike).unwrap().count(), 0);
    assert!(f.in_quarantine());
    assert_eq!(f.status(id), JournalStatus::Done);
    assert!(crate::undo_offered(
        &f.db.journal_entry(id).unwrap().unwrap()
    ));

    // The archive back in its place: the same entry comes home.
    f.archive_back(&aside);
    let done = crate::undo(&f.db, id).unwrap();
    assert_eq!(done.files_back, 2);
    assert!(f.at_home());
    assert_eq!(fs::read_dir(&look_alike).unwrap().count(), 0);
}

/// A link at the folder itself, and a file in place of a folder above it:
/// each refused with the place named, nothing moved, nothing replaced.
#[test]
fn a_link_or_a_file_on_the_way_home_is_refused_by_name() {
    let f = Fixture::new();
    let id = f.row(false, true);
    let elsewhere = f.base.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::remove_dir(&f.dir).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &f.dir).unwrap();

    let shown = format!("{:#}", crate::undo(&f.db, id).unwrap_err());
    assert!(shown.contains("a symbolic link or a junction"), "{shown}");
    assert!(shown.contains(&f.dir.display().to_string()), "{shown}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(f.in_quarantine());
    assert_eq!(f.status(id), JournalStatus::Done);

    fs::remove_file(&f.dir).unwrap();
    let year = f.dir.parent().unwrap();
    fs::remove_dir(year).unwrap();
    fs::write(year, b"not a folder 1517").unwrap();
    let shown = format!("{:#}", crate::undo(&f.db, id).unwrap_err());
    // Already the reading of the place refuses it (not a folder on the way).
    assert!(shown.contains("cannot be read"), "{shown}");
    assert!(shown.contains(&year.display().to_string()), "{shown}");
    assert_eq!(fs::read(year).unwrap(), b"not a folder 1517");
    assert!(f.in_quarantine());
    assert_eq!(f.status(id), JournalStatus::Done);
}

#[test]
fn missing_folders_of_an_old_row_are_made_again_and_it_is_undone() {
    let f = Fixture::new();
    let id = f.row(false, true);
    fs::remove_dir(&f.dir).unwrap();
    fs::remove_dir(f.dir.parent().unwrap()).unwrap();

    let done = crate::undo(&f.db, id).unwrap();
    assert_eq!(done.files_back, 2);
    assert!(f.at_home());
    for d in [f.dir.parent().unwrap(), f.dir.as_path()] {
        assert!(fs::symlink_metadata(d).unwrap().is_dir(), "{}", d.display());
    }
    assert_eq!(f.status(id), JournalStatus::Undone);
}

/// A folder is made only once every member's way is checked: a refusal
/// leaves nothing new behind.
#[test]
fn a_refused_old_row_makes_no_folder() {
    let f = Fixture::new();
    let id = f.row(false, true);
    fs::remove_dir(&f.dir).unwrap();
    let (_aside, look_alike) = f.archive_replaced_by_link();
    fs::remove_dir(&look_alike).unwrap();

    let shown = format!("{:#}", crate::undo(&f.db, id).unwrap_err());
    assert!(shown.contains("a symbolic link or a junction"), "{shown}");
    assert!(!look_alike.exists(), "a folder was made: {shown}");
    assert!(f.in_quarantine());
    assert_eq!(f.status(id), JournalStatus::Done);
}

#[test]
fn an_interrupted_old_row_of_a_run_without_roots_is_reconciled_alike() {
    let f = Fixture::new();
    let id = f.row(false, false);
    let (aside, look_alike) = f.archive_replaced_by_link();

    let shown = format!("{:#}", crate::reconcile_undo(&f.db, id).unwrap_err());
    assert!(shown.contains("a symbolic link or a junction"), "{shown}");
    assert!(shown.contains(&f.archive.display().to_string()), "{shown}");
    assert_eq!(fs::read_dir(&look_alike).unwrap().count(), 0);
    assert!(f.in_quarantine());
    assert_eq!(f.status(id), JournalStatus::Pending);

    f.archive_back(&aside);
    fs::remove_dir(&f.dir).unwrap();
    let done = crate::reconcile_undo(&f.db, id).unwrap();
    assert_eq!(done.done.files_back, 2);
    assert!(f.at_home());
    assert_ne!(f.status(id), JournalStatus::Pending);
}

/// A row that recorded its folder's identity is judged by it, as before:
/// the archive reached through a link to itself is the same folder and the
/// row comes home, where the mount-point walk would have refused the link.
#[test]
fn a_row_with_a_folder_identity_is_not_judged_by_the_mount_point() {
    let f = Fixture::new();
    let id = f.row(true, true);
    let aside = f.base.join("archive-aside");
    fs::rename(&f.archive, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &f.archive).unwrap();

    let done = crate::undo(&f.db, id).unwrap();
    assert_eq!(done.files_back, 2);
    assert!(f.at_home());
    assert_eq!(f.status(id), JournalStatus::Undone);
}

/// A row from a run that did record roots keeps its own bound: a folder
/// under none of them is still refused (el-4r8yf B3-R2), not walked from
/// the mount point.
#[test]
fn an_old_row_of_a_run_with_roots_keeps_its_own_bound() {
    let f = Fixture::new();
    let id = f.row(false, true);
    f.db.conn
        .execute(
            "UPDATE runs SET roots = ?1 WHERE id = ?2",
            rusqlite::params![
                format!("[{:?}]", f.base.join("elsewhere").display().to_string()),
                f.run
            ],
        )
        .unwrap();

    let shown = format!("{:#}", crate::undo(&f.db, id).unwrap_err());
    assert!(
        shown.contains("is under none of the folders this entry's run scanned"),
        "{shown}"
    );
    assert!(f.in_quarantine());
    assert_eq!(f.status(id), JournalStatus::Done);
}
