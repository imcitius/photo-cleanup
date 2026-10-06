//! Deleting an orphan — a file in quarantine the journal never claimed —
//! deletes nothing (el-63ph1).
//!
//! An orphan is by definition something no `done` or `pending` row
//! recorded: there is no evidence of what it was when it arrived, so there
//! is nothing to prove what lies at its path against. It used to be deleted
//! recursively by path — a stranger's file, a folder with everything in it,
//! a link, a Lightroom catalogue. Each test puts one of those at an orphan
//! path in a disposable temporary folder and asks `abandon_orphan`, the
//! function the web job `quarantine-purge` runs.

use crate::*;
use pc_db::{Db, JournalStatus};
use std::os::unix::fs::symlink;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    q: PathBuf,
    db: Db,
    run: i64,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let archive = root.join("archive");
    let q = archive.join(pc_core::QUARANTINE_DIR);
    fs::create_dir_all(&q).unwrap();
    let db = Db::open(&root.join("pc.db")).unwrap();
    let run = db
        .start_run(&[archive.display().to_string()], "test")
        .unwrap();
    Fx {
        _tmp: tmp,
        root,
        q,
        db,
        run,
    }
}

impl Fx {
    fn abandon(&self, path: &Path) -> Result<OrphanKept> {
        abandon_orphan(
            &self.db,
            self.run,
            &path.display().to_string(),
            &pc_core::work::Control::default(),
        )
    }

    /// The single `abandon` row, its status and its history.
    fn record(&self) -> (JournalStatus, Vec<(String, String)>) {
        let id: i64 = self
            .db
            .conn
            .query_row("SELECT id FROM journal WHERE op='abandon'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let status = self.db.journal_entry(id).unwrap().unwrap().status;
        let events = self
            .db
            .journal_events(id)
            .unwrap()
            .into_iter()
            .map(|e| (e.phase, e.kind))
            .collect();
        (status, events)
    }

    /// Kept, said why, deleted nothing, and the history says so.
    fn assert_kept(&self, kept: &OrphanKept, why: OrphanWhy) {
        assert_eq!(kept.why, why, "{kept}");
        let shown = kept.to_string();
        assert!(shown.contains(&kept.path), "{shown}");
        assert!(
            shown.contains("by hand") || shown.contains("вручную"),
            "{shown}"
        );
        let (status, events) = self.record();
        assert_ne!(status, JournalStatus::Purged);
        assert!(
            events.contains(&("abandon".into(), "kept".into())),
            "{events:?}"
        );
    }
}

#[test]
fn an_orphan_file_with_no_recorded_evidence_is_kept() {
    // Left by a database that is gone, or put there by another program:
    // either way nothing recorded what it was.
    let f = fx();
    let orphan = f.q.join("2015").join("IMG_0001.jpg");
    fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    fs::write(&orphan, b"a photograph from 2015").unwrap();

    let kept = f.abandon(&orphan).unwrap();

    assert_eq!(fs::read(&orphan).unwrap(), b"a photograph from 2015");
    f.assert_kept(&kept, OrphanWhy::Unproven);
    assert_eq!(kept.bytes, 22);
}

#[test]
fn a_link_at_an_orphan_path_is_kept_and_so_is_what_it_points_to() {
    let f = fx();
    let outside = f.root.join("elsewhere.jpg");
    fs::write(&outside, b"the user's photograph").unwrap();
    let link = f.q.join("IMG_0002.jpg");
    symlink(&outside, &link).unwrap();

    let kept = f.abandon(&link).unwrap();

    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(&outside).unwrap(), b"the user's photograph");
    f.assert_kept(&kept, OrphanWhy::Unproven);
}

#[test]
fn a_folder_orphan_is_kept_with_everything_inside_it() {
    let f = fx();
    let folder = f.q.join("Old Album");
    fs::create_dir_all(folder.join("sub")).unwrap();
    fs::write(folder.join("a.jpg"), b"one").unwrap();
    fs::write(folder.join("sub").join("b.jpg"), b"two").unwrap();

    let kept = f.abandon(&folder).unwrap();

    assert_eq!(fs::read(folder.join("a.jpg")).unwrap(), b"one");
    assert_eq!(fs::read(folder.join("sub").join("b.jpg")).unwrap(), b"two");
    f.assert_kept(&kept, OrphanWhy::Folder);
}

#[test]
fn nothing_of_lightrooms_is_deleted_as_an_orphan() {
    // The user's decision of 2026-10-06: Lightroom catalogues are not to be
    // touched at all — a single file as much as a folder.
    let f = fx();
    let catalogue = f.q.join("Lightroom Catalog.lrcat");
    fs::write(&catalogue, b"SQLite format 3\0").unwrap();
    let previews = f.q.join("Lightroom Catalog Previews.lrdata");
    fs::create_dir_all(previews.join("0")).unwrap();
    fs::write(previews.join("0").join("x.lrprev"), b"preview").unwrap();

    let kept = f.abandon(&catalogue).unwrap();
    assert_eq!(kept.why, OrphanWhy::Lightroom, "{kept}");
    assert!(catalogue.exists());

    let kept = f.abandon(&previews.join("0").join("x.lrprev")).unwrap();
    assert_eq!(kept.why, OrphanWhy::Lightroom, "{kept}");
    let kept = f.abandon(&previews).unwrap();
    assert_eq!(kept.why, OrphanWhy::Lightroom, "{kept}");
    assert_eq!(
        fs::read(previews.join("0").join("x.lrprev")).unwrap(),
        b"preview"
    );
}

#[test]
fn an_orphan_that_is_already_gone_is_reported_not_counted() {
    let f = fx();
    let kept = f.abandon(&f.q.join("never.jpg")).unwrap();
    assert_eq!(kept.why, OrphanWhy::Unproven, "{kept}");
    assert_eq!(kept.bytes, 0);
}

#[test]
fn a_stop_asked_for_before_abandon_journals_nothing_and_deletes_nothing() {
    let f = fx();
    let orphan = f.q.join("IMG_0003.jpg");
    fs::write(&orphan, b"x").unwrap();
    let control = pc_core::work::Control::default();
    control
        .cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let e = abandon_orphan(&f.db, f.run, &orphan.display().to_string(), &control).unwrap_err();
    assert!(e.is::<pc_core::work::Cancelled>(), "{e:#}");
    assert!(orphan.exists());
    let rows: i64 =
        f.db.conn
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap();
    assert_eq!(rows, 0);
}
