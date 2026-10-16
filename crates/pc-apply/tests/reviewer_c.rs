//! Boundaries of organize and recovery on Windows C, adopted from review
//! el-28b92 (el-67ku) as regressions. Synthetic fixtures; no race hooks.
use pc_db::{Db, JournalStatus};
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Fx {
    tmp: tempfile::TempDir,
    root: PathBuf,
    old: PathBuf,
    new: PathBuf,
    db: Db,
    run: i64,
}
impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let old = root.join("archive/old");
        let new = root.join("archive/new");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        let db = Db::open(&root.join("test.db")).unwrap();
        let run = db
            .start_run(&[root.join("archive").display().to_string()], "review")
            .unwrap();
        Self {
            tmp,
            root,
            old,
            new,
            db,
            run,
        }
    }
    fn plan(&self) -> pc_organize::Move {
        let src = self.old.join("frame.arw");
        fs::write(&src, b"synthetic frame 6173").unwrap();
        let md = fs::metadata(&src).unwrap();
        let mtime = pc_core::time::mtime_unix(&md);
        let id = self
            .db
            .upsert_file(
                &pc_db::NewFile {
                    path: src.display().to_string(),
                    name: "frame.arw".into(),
                    size: md.len() as i64,
                    mtime,
                    ..Default::default()
                },
                self.run,
            )
            .unwrap();
        pc_organize::Move {
            file_id: id,
            src: src.display().to_string(),
            dst: self.new.join("frame.arw").display().to_string(),
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
    fn jid(&self) -> i64 {
        self.db
            .conn
            .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
            .unwrap()
    }
    fn rows(&self) -> i64 {
        self.db
            .conn
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap()
    }
}
fn link(to: &Path, at: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(to, at).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(to, at).unwrap();
}
fn snapshot(p: &Path) -> (Vec<u8>, String) {
    let f = fs::File::open(p).unwrap();
    let m = f.metadata().unwrap();
    #[allow(unused_mut)]
    let mut details = format!(
        "{:?}/{:?}/{:?}/{:?}",
        pc_core::proof::Proof::of_file(&f),
        m.permissions(),
        m.modified(),
        m.created()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        details.push_str(&format!(
            "/{}/{}/{}/{}",
            m.uid(),
            m.gid(),
            m.mode(),
            m.nlink()
        ));
    }
    (fs::read(p).unwrap(), details)
}
#[test]
fn organize_refuses_a_stable_symlink_replacing_the_planned_frame() {
    let f = Fx::new();
    let m = f.plan();
    fs::write(f.old.join("frame.xmp"), b"our sidecar").unwrap();
    let saved = f.root.join("saved-frame.arw");
    fs::rename(&m.src, &saved).unwrap();
    let foreign = f.root.join("foreign.arw");
    fs::copy(&saved, &foreign).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&foreign)
        .unwrap()
        .set_modified(fs::metadata(&saved).unwrap().modified().unwrap())
        .unwrap();
    link(&foreign, Path::new(&m.src));
    let before = snapshot(&foreign);
    let r = pc_apply::organize(&f.db, f.run, std::slice::from_ref(&m));
    assert_eq!(
        snapshot(&foreign),
        before,
        "foreign payload or metadata changed"
    );
    eprintln!(
        "report={r:?}; src_present={}; dst_is_link={}; rows={}",
        fs::symlink_metadata(&m.src).is_ok(),
        fs::symlink_metadata(&m.dst).is_ok_and(|x| x.file_type().is_symlink()),
        f.rows()
    );
    match r {
        Ok(r) => {
            assert_eq!(
                r.done.frames, 0,
                "a non-plain frame must refuse its whole unit"
            );
            assert!(!r.refused.is_empty());
        }
        Err(e) => assert!(format!("{e:#}").contains(&m.src)),
    }
    assert!(fs::symlink_metadata(&m.src)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(f.old.join("frame.xmp").exists());
    assert!(!f.new.join("frame.xmp").exists());
}
#[test]
fn pending_reconcile_refuses_replaced_home_then_returns_to_original_folder() {
    let f = Fx::new();
    let m = f.plan();
    fs::write(f.old.join("frame.xmp"), b"our sidecar").unwrap();
    f.db.conn.execute_batch("CREATE TRIGGER crash BEFORE UPDATE OF status ON journal BEGIN SELECT RAISE(ABORT, 'review crash'); END;").unwrap();
    let e = pc_apply::organize(&f.db, f.run, std::slice::from_ref(&m)).unwrap_err();
    assert!(pc_apply::stopped_run(&e).is_some());
    f.db.conn.execute_batch("DROP TRIGGER crash").unwrap();
    let jid = f.jid();
    let saved = f.root.join("original-folder");
    fs::rename(&f.old, &saved).unwrap();
    fs::create_dir(&f.old).unwrap();
    let foreign = f.old.join("foreign.dat");
    fs::write(&foreign, b"foreign payload 973").unwrap();
    let before = snapshot(&foreign);
    let e = pc_apply::reconcile_undo(&f.db, jid).unwrap_err();
    assert!(format!("{e:#}").contains(&f.old.display().to_string()));
    assert_eq!(snapshot(&foreign), before);
    assert_eq!(
        f.db.journal_entry(jid).unwrap().unwrap().status,
        JournalStatus::Pending
    );
    assert!(Path::new(&m.dst).exists());
    assert!(f.new.join("frame.xmp").exists());
    let kept = f.root.join("foreign-folder-kept");
    fs::rename(&f.old, &kept).unwrap();
    fs::rename(&saved, &f.old).unwrap();
    let r = pc_apply::reconcile_undo(&f.db, jid).unwrap();
    assert_eq!(r.done.files_back, 2);
    assert_eq!(snapshot(&kept.join("foreign.dat")), before);
    assert!(Path::new(&m.src).exists());
    assert!(f.old.join("frame.xmp").exists());
}
#[test]
fn pending_reconcile_rejects_identical_foreign_file_without_touching_metadata() {
    let f = Fx::new();
    let m = f.plan();
    f.db.conn.execute_batch("CREATE TRIGGER crash BEFORE UPDATE OF status ON journal BEGIN SELECT RAISE(ABORT, 'review crash'); END;").unwrap();
    assert!(pc_apply::organize(&f.db, f.run, std::slice::from_ref(&m)).is_err());
    f.db.conn.execute_batch("DROP TRIGGER crash").unwrap();
    let jid = f.jid();
    let saved = f.root.join("owned-saved");
    fs::rename(&m.dst, &saved).unwrap();
    fs::copy(&saved, &m.dst).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&m.dst)
        .unwrap()
        .set_modified(fs::metadata(&saved).unwrap().modified().unwrap())
        .unwrap();
    let before = snapshot(Path::new(&m.dst));
    let e = pc_apply::reconcile_undo(&f.db, jid).unwrap_err();
    assert!(format!("{e:#}").contains(&m.dst));
    assert_eq!(snapshot(Path::new(&m.dst)), before);
    assert_eq!(
        f.db.journal_entry(jid).unwrap().unwrap().status,
        JournalStatus::Pending
    );
    assert!(!Path::new(&m.src).exists());
    let kept = f.root.join("foreign-kept");
    fs::rename(&m.dst, &kept).unwrap();
    fs::rename(&saved, &m.dst).unwrap();
    assert_eq!(
        pc_apply::reconcile_undo(&f.db, jid)
            .unwrap()
            .done
            .files_back,
        1
    );
    assert_eq!(snapshot(&kept), before);
}
#[test]
fn organize_sidecar_collision_preserves_the_complete_unit_and_foreign_metadata() {
    let f = Fx::new();
    let m = f.plan();
    fs::write(f.old.join("frame.xmp"), b"our sidecar").unwrap();
    let foreign = f.new.join("frame.xmp");
    fs::write(&foreign, b"foreign edits 9841").unwrap();
    let before = snapshot(&foreign);
    let frame = snapshot(Path::new(&m.src));
    let r = pc_apply::organize(&f.db, f.run, std::slice::from_ref(&m)).unwrap();
    assert_eq!(r.done.frames, 0);
    assert!(r.refused[0].1.contains(&foreign.display().to_string()));
    assert_eq!(snapshot(&foreign), before);
    assert_eq!(snapshot(Path::new(&m.src)), frame);
    assert!(!Path::new(&m.dst).exists());
    assert_eq!(f.rows(), 0);
}
#[test]
fn refused_orphans_remain_recoverable_and_abandon_preserves_foreign_payload() {
    let f = Fx::new();
    let q = f.old.join(pc_core::QUARANTINE_DIR);
    fs::create_dir(&q).unwrap();
    let payload = f.root.join("foreign.dat");
    fs::write(&payload, b"foreign 834").unwrap();
    let before = snapshot(&payload);
    let at = q.join("orphan.arw");
    let dst = f.old.join("orphan.arw");
    link(&payload, &at);
    let e = pc_apply::adopt_orphan(
        &f.db,
        f.run,
        &at.display().to_string(),
        &dst.display().to_string(),
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains(&at.display().to_string()));
    assert_eq!(f.rows(), 0);
    let kept =
        pc_apply::abandon_orphan(&f.db, f.run, &at.display().to_string(), &Default::default())
            .unwrap();
    assert!(kept.to_string().contains(&at.display().to_string()));
    assert_eq!(snapshot(&payload), before);
    assert!(fs::symlink_metadata(&at).unwrap().file_type().is_symlink());
    assert!(!dst.exists());
    fs::rename(&at, q.join("link-retained")).unwrap();
    fs::write(&at, b"recoverable orphan 973").unwrap();
    assert_eq!(
        pc_apply::adopt_orphan(
            &f.db,
            f.run,
            &at.display().to_string(),
            &dst.display().to_string()
        )
        .unwrap()
        .files_back,
        1
    );
    pc_apply::undo(&f.db, f.jid()).unwrap();
    assert_eq!(fs::read(&at).unwrap(), b"recoverable orphan 973");
    assert_eq!(snapshot(&payload), before);
    assert!(f.tmp.path().exists());
}
