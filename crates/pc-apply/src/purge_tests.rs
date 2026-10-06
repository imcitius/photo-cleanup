//! `purge` deletes only what it proves each entry moved (el-3s9kp).
//!
//! Every test goes through the real consumer — a real quarantine by
//! `quarantine_file`/`quarantine`, then `crate::purge`, the function
//! `photo-cleanup derived purge` runs — on disposable files in a temporary
//! folder. What is put in the way is what another program could leave at a
//! recorded path: another file, a link, a folder, a quarantine folder
//! replaced by a link. Each one used to be deleted (el-lvtmk D6/S2).

use crate::*;
use pc_db::{model::NewBundle, Db, JournalStatus};
use pc_family::plan::Candidate;
use std::os::unix::fs::symlink;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    archive: PathBuf,
    q: PathBuf,
    db: Db,
    run: i64,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let archive = root.join("archive");
    fs::create_dir_all(&archive).unwrap();
    let db = Db::open(&root.join("pc.db")).unwrap();
    let run = db
        .start_run(&[archive.display().to_string()], "test")
        .unwrap();
    Fx {
        q: archive.join(pc_core::QUARANTINE_DIR),
        _tmp: tmp,
        root,
        archive,
        db,
        run,
    }
}

impl Fx {
    /// A photograph (with whatever sidecars are already beside it) moved
    /// into quarantine by the real apply; the journal entry's id.
    fn quarantined(&self, name: &str, bytes: &[u8]) -> i64 {
        let path = self.archive.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        let file_id = self
            .db
            .upsert_file(
                &pc_db::NewFile {
                    path: path.display().to_string(),
                    name: name.into(),
                    size: bytes.len() as i64,
                    ..Default::default()
                },
                self.run,
            )
            .unwrap();
        let c = Candidate {
            file_id,
            family_id: 0,
            path: path.display().to_string(),
            size: bytes.len() as i64,
            role: pc_family::Role::Copy,
            keeper_id: 0,
            keeper_path: String::new(),
            reason: "выбор человека".into(),
            manual: true,
            group_keeper: String::new(),
        };
        let filed = files::quarantine_file(&self.db, self.run, &c, None).unwrap();
        assert_eq!(filed.outcome, FileOutcome::Moved, "{}", filed.why);
        self.db
            .journal_quarantined(None)
            .unwrap()
            .into_iter()
            .find(|e| e.src == path.display().to_string())
            .unwrap()
            .id
    }

    /// A folder of previews: three files, two levels of subfolders,
    /// scanned and moved into quarantine whole by the real apply.
    fn bundle(&self, name: &str) -> (i64, PathBuf) {
        // Lightroom's own format: every `.lrprev` starts `AgHg`. The sizes
        // are those the bundle tests count on.
        self.bundle_of(
            name,
            pc_core::DerivedKind::LrPreviews,
            &[
                ("root.lrprev", b"AgHg preview 0".to_vec()),
                ("1/a.lrprev", b"AgHg prev 1".to_vec()),
                ("1/2/b.lrprev", b"AgHg preview 2 deep".to_vec()),
            ],
        )
    }

    /// A bundle of `kind` holding `files`, scanned and moved into
    /// quarantine by the real apply: the entry and where the bundle is now.
    fn bundle_of(
        &self,
        name: &str,
        kind: pc_core::DerivedKind,
        files: &[(&str, Vec<u8>)],
    ) -> (i64, PathBuf) {
        let dir = self.archive.join(name);
        fs::create_dir_all(&dir).unwrap();
        for (rel, bytes) in files {
            let p = dir.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, bytes).unwrap();
        }
        let (count, size, newest) = dir_stats(&dir);
        self.moved_bundle(&dir, true, kind, count as i64, size as i64, newest)
    }

    /// A bundle that is one file (system junk), moved by the real apply.
    fn junk_file(&self, name: &str, bytes: &[u8]) -> (i64, PathBuf) {
        let path = self.archive.join(name);
        fs::write(&path, bytes).unwrap();
        let md = fs::metadata(&path).unwrap();
        self.moved_bundle(
            &path,
            false,
            pc_core::DerivedKind::SystemJunk,
            1,
            md.len() as i64,
            pc_core::time::mtime_unix(&md),
        )
    }

    fn moved_bundle(
        &self,
        path: &Path,
        is_dir: bool,
        kind: pc_core::DerivedKind,
        file_count: i64,
        size: i64,
        newest_mtime: i64,
    ) -> (i64, PathBuf) {
        self.db
            .upsert_bundle(
                &NewBundle {
                    path: path.display().to_string(),
                    is_dir,
                    disk: "root".into(),
                    dev: 0,
                    mount: self.archive.display().to_string(),
                    kind,
                    owner_ref: None,
                    file_count,
                    size,
                    newest_mtime,
                },
                self.run,
            )
            .unwrap();
        let b = self
            .db
            .list_bundles(&Default::default())
            .unwrap()
            .into_iter()
            .find(|b| b.path == path.display().to_string())
            .unwrap();
        assert_eq!(
            quarantine(&self.db, self.run, &b, None).unwrap(),
            Outcome::Moved
        );
        let id = self
            .db
            .journal_quarantined(None)
            .unwrap()
            .into_iter()
            .find(|e| e.src == path.display().to_string())
            .unwrap()
            .id;
        (id, self.q.join(path.file_name().unwrap()))
    }

    /// Set the object the entry moved aside, outside quarantine, so a test
    /// can put something else at its recorded place.
    fn set_aside(&self, at: &Path) -> PathBuf {
        let aside = self.root.join(format!(
            "aside-{}",
            at.file_name().unwrap().to_string_lossy()
        ));
        fs::rename(at, &aside).unwrap();
        aside
    }

    fn status(&self, id: i64) -> JournalStatus {
        self.db.journal_entry(id).unwrap().unwrap().status
    }

    fn events(&self, id: i64) -> Vec<(String, String)> {
        self.db
            .journal_events(id)
            .unwrap()
            .into_iter()
            .map(|e| (e.phase, e.kind))
            .collect()
    }

    /// Kept by purge whatever its proof (el-3s9kp: no bundle, nothing of
    /// Lightroom's): nothing went, nothing was refused as unproven, the
    /// entry is still quarantine (undoable), its history says it was kept,
    /// and the words name it and say a person may delete it by hand.
    fn assert_kept(&self, id: i64, t: &Totals) {
        assert_eq!((t.bundles, t.files, t.bytes), (0, 0, 0), "{:?}", t.kept);
        assert!(t.skipped.is_empty() && t.stopped.is_empty(), "{t:?}");
        assert_eq!(t.kept.len(), 1, "{:?}", t.kept);
        assert!(
            t.kept[0].contains("delete it by hand if you are sure"),
            "{:?}",
            t.kept
        );
        let e = self.db.journal_entry(id).unwrap().unwrap();
        assert!(
            t.kept[0].contains(e.dst.as_deref().unwrap()),
            "{:?}",
            t.kept
        );
        assert_eq!(e.status, JournalStatus::Done);
        let ev = self.events(id);
        assert!(ev.contains(&("purge".into(), "kept".into())), "{ev:?}");
        assert!(!ev.contains(&("purge".into(), "begun".into())), "{ev:?}");
    }

    /// Refused whole: nothing went, the entry is still quarantine (undoable),
    /// and its history says so.
    fn assert_refused(&self, id: i64, t: &Totals) {
        assert_eq!((t.bundles, t.files, t.bytes), (0, 0, 0), "{:?}", t.skipped);
        assert_eq!(t.skipped.len(), 1, "{:?}", t.skipped);
        assert_eq!(self.status(id), JournalStatus::Done);
        assert!(
            self.events(id)
                .contains(&("purge".into(), "refused".into())),
            "{:?}",
            self.events(id)
        );
    }
}

#[test]
fn a_stranger_at_the_recorded_quarantine_path_is_not_deleted() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"the copy that was moved");
    let at = f.q.join("frame.jpg");
    let ours = f.set_aside(&at);
    fs::write(&at, b"someone else's photograph").unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(fs::read(&at).unwrap(), b"someone else's photograph");
    assert_eq!(fs::read(&ours).unwrap(), b"the copy that was moved");
    f.assert_refused(id, &t);
    assert!(t.skipped[0].contains("frame.jpg"), "{:?}", t.skipped);
}

#[test]
fn a_link_at_the_recorded_path_is_neither_followed_nor_deleted() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    let at = f.q.join("frame.jpg");
    f.set_aside(&at);
    let target = f.root.join("elsewhere.jpg");
    fs::write(&target, b"the link's target").unwrap();
    symlink(&target, &at).unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert!(fs::symlink_metadata(&at).unwrap().file_type().is_symlink());
    assert_eq!(fs::read(&target).unwrap(), b"the link's target");
    f.assert_refused(id, &t);
}

#[test]
fn a_folder_in_a_files_place_is_not_emptied() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    let at = f.q.join("frame.jpg");
    f.set_aside(&at);
    fs::create_dir_all(at.join("album")).unwrap();
    fs::write(at.join("album/inside.jpg"), b"a photograph in a folder").unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(
        fs::read(at.join("album/inside.jpg")).unwrap(),
        b"a photograph in a folder"
    );
    f.assert_refused(id, &t);
}

#[test]
fn a_quarantine_folder_replaced_by_a_link_is_not_followed() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    // The whole quarantine folder is moved away and a link to another
    // folder, holding a file of the same name, takes its place.
    let moved = f.root.join("moved-quarantine");
    fs::rename(&f.q, &moved).unwrap();
    let other = f.root.join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("frame.jpg"), b"not ours").unwrap();
    symlink(&other, &f.q).unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(fs::read(other.join("frame.jpg")).unwrap(), b"not ours");
    assert_eq!(fs::read(moved.join("frame.jpg")).unwrap(), b"moved");
    f.assert_refused(id, &t);
}

#[test]
fn a_quarantine_folder_replaced_by_another_folder_is_not_trusted() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    let moved = f.root.join("moved-quarantine");
    fs::rename(&f.q, &moved).unwrap();
    fs::create_dir_all(&f.q).unwrap();
    fs::write(f.q.join("frame.jpg"), b"moved").unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(fs::read(f.q.join("frame.jpg")).unwrap(), b"moved");
    assert_eq!(fs::read(moved.join("frame.jpg")).unwrap(), b"moved");
    f.assert_refused(id, &t);
}

#[test]
fn a_companion_that_does_not_prove_keeps_its_whole_unit() {
    let f = fx();
    fs::write(f.archive.join("frame.xmp"), b"edits").unwrap();
    let id = f.quarantined("frame.arw", b"raw frame");
    let side = f.q.join("frame.xmp");
    f.set_aside(&side);
    fs::write(&side, b"someone else's edits").unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(fs::read(f.q.join("frame.arw")).unwrap(), b"raw frame");
    assert_eq!(fs::read(&side).unwrap(), b"someone else's edits");
    f.assert_refused(id, &t);
    assert!(t.skipped[0].contains("frame.xmp"), "{:?}", t.skipped);
}

#[test]
fn an_entry_with_a_located_object_is_refused_whole() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    let e = f.db.journal_entry(id).unwrap().unwrap();
    let m = &e.manifest[0];
    f.db.journal_event_located(
        id,
        "forward",
        "located",
        "",
        &[pc_db::Located {
            src: m.src.clone(),
            dst: m.dst.clone(),
            role: "checked".into(),
            held: true,
            at: pc_core::whereabouts::Whereabouts::uncertain(None, "the folder moved"),
            proof: None,
        }],
    )
    .unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(fs::read(f.q.join("frame.jpg")).unwrap(), b"moved");
    f.assert_refused(id, &t);
}

#[test]
fn a_proven_entry_goes_whole_and_says_what_went() {
    let f = fx();
    fs::write(f.archive.join("frame.xmp"), b"edits").unwrap();
    let id = f.quarantined("frame.arw", b"raw frame");

    let t = purge(&f.db, 0).unwrap();

    assert!(t.skipped.is_empty(), "{:?}", t.skipped);
    assert_eq!((t.bundles, t.files, t.bytes), (1, 2, 14));
    assert!(!f.q.join("frame.arw").exists());
    assert!(!f.q.join("frame.xmp").exists());
    // The quarantine folder itself is the tool's, and is left.
    assert!(f.q.is_dir());
    assert_eq!(f.status(id), JournalStatus::Purged);
    let ev = f.events(id);
    assert!(ev.contains(&("purge".into(), "begun".into())), "{ev:?}");
    assert!(ev.contains(&("purge".into(), "done".into())), "{ev:?}");
}

#[test]
fn a_hash_on_record_decides_over_matching_metadata() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    let at = f.q.join("frame.jpg");
    // The recorded evidence with a content hash, written as the journal
    // keeps it (no hash is journaled today; the check is there for when one
    // is). BLAKE3 of the bytes, worked out once.
    const OF_MOVED: &str = "51ee91ced7437f101da3822e401156f52652229e1c8ae48ab5d7b22c0764393b";
    const OF_OTHER: &str = "3f796163ebf94718de1cd7582655c012f995c06f1e6970ea2bdc15bcd88a324a";
    let e = f.db.journal_entry(id).unwrap().unwrap();
    let (m, p) = (&e.manifest[0], e.manifest[0].proof.clone().unwrap());
    let record = |hash: &str| {
        let birth = p
            .birth_ns
            .map_or(String::new(), |b| format!(",\"birth_ns\":{b}"));
        let json = format!(
            "[{{\"src\":{:?},\"dst\":{:?},\"proof\":{{\"v\":{},\"dev\":{},\"ino\":{},\"kind\":\"file\",\"size\":{},\"mtime_ns\":{}{birth},\"blake3\":\"{hash}\"}}}}]",
            m.src,
            m.dst,
            p.v,
            p.dev,
            p.ino,
            p.size.unwrap(),
            p.mtime_ns.unwrap()
        );
        f.db.conn
            .execute(
                "UPDATE journal SET manifest=?1 WHERE id=?2",
                rusqlite::params![json, id],
            )
            .unwrap();
        let back = f.db.journal_entry(id).unwrap().unwrap();
        assert_eq!(
            back.manifest[0].proof.as_ref().unwrap().blake3.as_deref(),
            Some(hash)
        );
    };
    record(OF_OTHER);
    let t = purge(&f.db, 0).unwrap();
    assert_eq!(fs::read(&at).unwrap(), b"moved");
    f.assert_refused(id, &t);

    record(OF_MOVED);
    let t = purge(&f.db, 0).unwrap();
    assert_eq!((t.bundles, t.files), (1, 1), "{:?}", t.skipped);
    assert!(!at.exists());
}

/// The user's decision of 2026-10-06: a bundle every proof of which still
/// matches — the case purge used to delete — is kept, named with where it
/// is and how big it was, and nothing beside it is touched either.
#[test]
fn a_proven_bundle_is_kept_and_named_with_its_size() {
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    let beside = f.q.join("not-the-bundle.jpg");
    fs::write(&beside, b"kept").unwrap();

    let t = purge(&f.db, 0).unwrap();

    f.assert_kept(id, &t);
    assert!(t.kept[0].contains("44 B"), "{:?}", t.kept);
    assert!(t.kept[0].contains("files when it moved: 3"), "{:?}", t.kept);
    for (rel, bytes) in [
        ("root.lrprev", &b"AgHg preview 0"[..]),
        ("1/a.lrprev", b"AgHg prev 1"),
        ("1/2/b.lrprev", b"AgHg preview 2 deep"),
    ] {
        assert_eq!(fs::read(at.join(rel)).unwrap(), bytes, "{rel}");
    }
    assert_eq!(fs::read(&beside).unwrap(), b"kept");

    // Kept every time it is asked, each time on record; and still undoable.
    let t = purge(&f.db, 0).unwrap();
    f.assert_kept(id, &t);
    let kept = f.events(id).iter().filter(|e| e.1 == "kept").count();
    assert_eq!(kept, 2);
    undo(&f.db, id).unwrap();
    assert!(f.archive.join("Cat Previews.lrdata/1/2/b.lrprev").exists());
}

#[test]
fn a_bundle_holding_more_than_was_moved_is_not_deleted() {
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    fs::write(at.join("1/2/photo.jpg"), b"dropped in later").unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(
        fs::read(at.join("1/2/photo.jpg")).unwrap(),
        b"dropped in later"
    );
    assert!(at.join("1/a.lrprev").exists());
    f.assert_kept(id, &t);
}

#[test]
fn a_link_inside_a_bundle_refuses_it_and_its_target_stays() {
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    // Swap one preview for a link of the same size story: the counts no
    // longer match, and in any case a link is not deleted as content.
    let target = f.root.join("target.jpg");
    fs::write(&target, b"a photograph").unwrap();
    fs::remove_file(at.join("root.lrprev")).unwrap();
    symlink(&target, at.join("root.lrprev")).unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"a photograph");
    assert!(at.join("1/2/b.lrprev").exists());
    f.assert_kept(id, &t);
}

#[test]
fn another_folder_at_a_bundles_place_is_not_deleted() {
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    let ours = f.set_aside(&at);
    // Same shape, same bytes: only its identity differs.
    for (rel, bytes) in [
        ("root.lrprev", &b"AgHg preview 0"[..]),
        ("1/a.lrprev", b"AgHg prev 1"),
        ("1/2/b.lrprev", b"AgHg preview 2 deep"),
    ] {
        let p = at.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    let t = purge(&f.db, 0).unwrap();

    assert!(at.join("1/2/b.lrprev").exists());
    assert!(ours.join("1/2/b.lrprev").exists());
    f.assert_kept(id, &t);
}

#[test]
fn a_swap_in_the_last_instant_is_detected_and_reported_not_hidden() {
    // The residual POSIX leaves: no conditional unlink. Something put under
    // the name exactly between the last comparison and `unlinkat` goes
    // instead. Outside the threat model (deliberate same-user interleaving)
    // — but when it happens, the held descriptor says so and the entry is
    // not closed as purged.
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    let at = f.q.join("frame.jpg");
    let aside = f.root.join("ours-aside.jpg");
    let (a2, at2) = (aside.clone(), at.clone());
    let _g = crate::purge::seam::before_unlink(move |p| {
        if p == at2 {
            fs::rename(&at2, &a2).unwrap();
            fs::write(&at2, b"stranger").unwrap();
        }
    });

    let err = purge_entry(&f.db, id).unwrap_err();

    let stop = err.downcast_ref::<PurgeStopped>().expect("typed stop");
    assert_eq!(stop.done.purged_files, 0);
    assert!(format!("{err:#}").contains("frame.jpg"), "{err:#}");
    assert_eq!(
        fs::read(&aside).unwrap(),
        b"moved",
        "the checked file stays"
    );
    assert_eq!(f.status(id), JournalStatus::Pending);
    assert!(f.events(id).contains(&("purge".into(), "partial".into())));
}

#[test]
fn a_row_without_evidence_is_left_for_a_person() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    f.db.conn
        .execute("UPDATE journal SET manifest=NULL WHERE id=?1", [id])
        .unwrap();

    let t = purge(&f.db, 0).unwrap();

    assert_eq!(fs::read(f.q.join("frame.jpg")).unwrap(), b"moved");
    f.assert_refused(id, &t);
}

#[test]
fn reviewer_same_size_foreign_photo_inside_bundle_must_survive() {
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    let old = at.join("root.lrprev");
    let saved = f.root.join("saved-preview");
    fs::rename(&old, &saved).unwrap();
    let photo = at.join("original.jpg");
    fs::write(&photo, b"FOREIGN PHOTO!").unwrap(); // same 14 bytes, new inode and name
    let md = fs::metadata(&photo).unwrap();
    let t = purge(&f.db, 0).unwrap();
    assert!(
        photo.exists(),
        "foreign photo installed before purge was deleted: {t:?}"
    );
    assert_eq!(fs::read(&photo).unwrap(), b"FOREIGN PHOTO!");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(fs::metadata(&photo).unwrap().ino(), md.ino());
    f.assert_kept(id, &t);
}

#[test]
fn reviewer_foreign_hardlink_preserves_payload_identity_and_permissions() {
    use std::os::unix::fs::MetadataExt;
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    let at = f.q.join("frame.jpg");
    f.set_aside(&at);
    let foreign = f.root.join("foreign.jpg");
    fs::write(&foreign, b"foreign payload").unwrap();
    fs::hard_link(&foreign, &at).unwrap();
    let before = fs::metadata(&at).unwrap();
    let t = purge(&f.db, 0).unwrap();
    let after = fs::metadata(&at).unwrap();
    assert_eq!(fs::read(&at).unwrap(), b"foreign payload");
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.nlink(),
            before.mode(),
            before.uid(),
            before.gid()
        ),
        (
            after.dev(),
            after.ino(),
            after.nlink(),
            after.mode(),
            after.uid(),
            after.gid()
        )
    );
    f.assert_refused(id, &t);
}

#[test]
fn reviewer_fifo_is_refused_without_blocking_or_changing_metadata() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    let at = f.q.join("frame.jpg");
    f.set_aside(&at);
    let c = std::ffi::CString::new(at.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc_for_test_mkfifo(c.as_ptr(), 0o600) }, 0);
    let before = fs::symlink_metadata(&at).unwrap();
    let t = purge(&f.db, 0).unwrap();
    let after = fs::symlink_metadata(&at).unwrap();
    assert!(after.file_type().is_fifo());
    assert_eq!(
        (before.ino(), before.mode(), before.uid(), before.gid()),
        (after.ino(), after.mode(), after.uid(), after.gid())
    );
    f.assert_refused(id, &t);
}
unsafe extern "C" {
    #[link_name = "mkfifo"]
    fn libc_for_test_mkfifo(path: *const std::ffi::c_char, mode: u32) -> i32;
}

#[test]
fn reviewer_unreadable_manifest_refusal_is_journaled() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    f.db.conn
        .execute(
            "UPDATE journal SET manifest='[{\"future_format\":true}]' WHERE id=?1",
            [id],
        )
        .unwrap();
    let t = purge(&f.db, 0).unwrap();
    assert_eq!(fs::read(f.q.join("frame.jpg")).unwrap(), b"moved");
    f.assert_refused(id, &t);
}

#[test]
fn reviewer_final_journal_failure_reports_actual_deletions() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    f.db.conn.execute_batch("CREATE TRIGGER fail_purge_close BEFORE UPDATE OF status ON journal WHEN NEW.status='purged' BEGIN SELECT RAISE(FAIL, 'synthetic journal failure'); END;").unwrap();
    let t = purge(&f.db, 0).unwrap();
    assert!(!f.q.join("frame.jpg").exists());
    assert_eq!(
        (t.files, t.bytes),
        (1, 5),
        "deleted bytes were hidden: {t:?}"
    );
    assert_eq!(f.status(id), JournalStatus::Pending);
}

#[test]
fn reviewer_hardlinked_owned_file_reports_zero_reclaimed_bytes() {
    let f = fx();
    f.quarantined("frame.jpg", b"moved");
    let alias = f.root.join("alias.jpg");
    fs::hard_link(f.q.join("frame.jpg"), &alias).unwrap();
    let t = purge(&f.db, 0).unwrap();
    assert_eq!((t.files, t.bytes), (1, 0));
    assert_eq!(fs::read(alias).unwrap(), b"moved");
}

#[test]
fn reviewer_foreign_entries_and_parents_preserve_payload_and_metadata() {
    use std::os::unix::fs::MetadataExt;
    for case in ["file", "link", "directory", "parent", "root"] {
        let f = fx();
        fs::create_dir_all(f.archive.join("nested")).unwrap();
        let id = f.quarantined("nested/frame.jpg", b"owned");
        let recorded = PathBuf::from(f.db.journal_entry(id).unwrap().unwrap().dst.unwrap());
        let target = f.root.join("foreign-target");
        fs::write(&target, b"foreign target").unwrap();
        match case {
            "parent" | "root" => {
                let q = recorded.parent().unwrap();
                let dir = if case == "root" {
                    q.to_path_buf()
                } else {
                    q.parent().unwrap().to_path_buf()
                };
                fs::rename(&dir, f.root.join("held-original-tree")).unwrap();
                fs::create_dir_all(recorded.parent().unwrap()).unwrap();
                fs::write(&recorded, b"foreign object").unwrap();
            }
            _ => {
                f.set_aside(&recorded);
                match case {
                    "file" => fs::write(&recorded, b"foreign object").unwrap(),
                    "link" => symlink(&target, &recorded).unwrap(),
                    "directory" => {
                        fs::create_dir(&recorded).unwrap();
                        fs::write(recorded.join("photo.jpg"), b"foreign object").unwrap();
                    }
                    _ => unreachable!(),
                }
            }
        }
        let fingerprint = |p: &Path| {
            let m = fs::symlink_metadata(p).unwrap();
            (
                m.dev(),
                m.ino(),
                m.mode(),
                m.uid(),
                m.gid(),
                m.nlink(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
            )
        };
        let before = fingerprint(&recorded);
        let t = purge(&f.db, 0).unwrap();
        assert_eq!(before, fingerprint(&recorded), "case {case}");
        assert_eq!(fs::read(&target).unwrap(), b"foreign target");
        if case == "link" {
            assert_eq!(fs::read_link(&recorded).unwrap(), target);
        } else if case == "directory" {
            assert_eq!(
                fs::read(recorded.join("photo.jpg")).unwrap(),
                b"foreign object"
            );
        } else {
            assert_eq!(fs::read(&recorded).unwrap(), b"foreign object");
        }
        f.assert_refused(id, &t);
    }
}

#[test]
fn reviewer_one_unproven_companion_refuses_entire_frame_unit() {
    let f = fx();
    fs::write(f.archive.join("frame.xmp"), b"edits").unwrap();
    let id = f.quarantined("frame.jpg", b"owned frame");
    let at = f.q.join("frame.xmp");
    let aside = f.set_aside(&at);
    fs::write(&at, b"foreign edits").unwrap();
    let t = purge(&f.db, 0).unwrap();
    assert_eq!(fs::read(f.q.join("frame.jpg")).unwrap(), b"owned frame");
    assert_eq!(fs::read(&at).unwrap(), b"foreign edits");
    assert_eq!(fs::read(aside).unwrap(), b"edits");
    f.assert_refused(id, &t);
}

#[test]
fn reviewer_new_entry_remains_during_retention() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"owned frame");
    let before = f.events(id);
    let t = purge(&f.db, 604800).unwrap();
    assert_eq!((t.bundles, t.files, t.bytes), (0, 0, 0));
    assert_eq!(fs::read(f.q.join("frame.jpg")).unwrap(), b"owned frame");
    assert_eq!(before, f.events(id));
}

// --- el-8s63g B1: a bundle goes only against its list, and only if all of
// it is something its owner rebuilds. ---

/// A real, decodable JPEG (synthetic pixels).
fn jpeg(shade: u8) -> Vec<u8> {
    let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8, y as u8, shade]));
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut out)
        .encode_image(&img)
        .unwrap();
    out
}

fn preview(tag: &str) -> Vec<u8> {
    format!("AgHg{tag}").into_bytes()
}

/// A real SQLite 3 header prefix (4096-byte pages, payload fractions
/// 64/32/32), then synthetic bytes.
const SQLITE_HEAD: &[u8] = b"SQLite format 3\0\x10\x00\x01\x01\x00\x40\x20\x20 synthetic index";

/// Kept whole, and every file of the bundle is still there, byte for byte.
fn assert_bundle_kept(f: &Fx, id: i64, t: &Totals, at: &Path, files: &[(&str, Vec<u8>)]) {
    f.assert_kept(id, t);
    for (rel, bytes) in files {
        assert_eq!(&fs::read(at.join(rel)).unwrap(), bytes, "{rel}");
    }
}

#[test]
fn a_photograph_that_moved_with_a_preview_bundle_is_never_deleted_with_it() {
    // The list matches exactly — the photograph was in the folder when it
    // moved — and still nothing goes: a JPEG is not a preview cache.
    let f = fx();
    let files = vec![
        ("root.lrprev", preview("0")),
        ("1/original.jpg", jpeg(37)),
        ("previews.db", SQLITE_HEAD.to_vec()),
    ];
    let (id, at) = f.bundle_of(
        "Cat Previews.lrdata",
        pc_core::DerivedKind::LrPreviews,
        &files,
    );
    let t = purge(&f.db, 0).unwrap();
    assert_bundle_kept(&f, id, &t, &at, &files);
}

#[test]
fn a_photograph_named_as_a_preview_is_known_by_its_bytes() {
    let f = fx();
    let files = vec![("root.lrprev", preview("0")), ("1/a.lrprev", jpeg(90))];
    let (id, at) = f.bundle_of(
        "Cat Previews.lrdata",
        pc_core::DerivedKind::LrPreviews,
        &files,
    );
    let t = purge(&f.db, 0).unwrap();
    assert_bundle_kept(&f, id, &t, &at, &files);
}

#[test]
fn sidecars_catalogue_data_and_unknown_files_keep_the_bundle() {
    for extra in [
        ("frame.xmp", b"<x:xmpmeta/>".to_vec()),
        ("._frame", b"\0\x05\x16\x07 apple double".to_vec()),
        ("Cat.lrcat-data/mask.bin", b"ai masks".to_vec()),
        ("previews.db-journal", SQLITE_HEAD.to_vec()),
        ("notes.txt", b"what is this".to_vec()),
        ("2/b.lrprev", b"right name, wrong bytes".to_vec()),
    ] {
        let f = fx();
        let files = vec![("root.lrprev", preview("0")), extra.clone()];
        let (id, at) = f.bundle_of(
            "Cat Previews.lrdata",
            pc_core::DerivedKind::LrPreviews,
            &files,
        );
        let t = purge(&f.db, 0).unwrap();
        assert_bundle_kept(&f, id, &t, &at, &files);
    }
}

#[test]
fn smart_previews_are_dng_and_are_not_deleted_automatically() {
    let f = fx();
    let dng = [&b"II\x2a\x00"[..], &[0u8; 60]].concat();
    let files = vec![("A/1234.dng", dng)];
    let (id, at) = f.bundle_of(
        "Cat Smart Previews.lrdata",
        pc_core::DerivedKind::LrSmartPreviews,
        &files,
    );
    let t = purge(&f.db, 0).unwrap();
    assert_bundle_kept(&f, id, &t, &at, &files);
}

#[test]
fn a_preview_replaced_by_another_of_the_same_name_size_and_format_is_refused() {
    // Everything a name, a count, a size or a magic number can say is the
    // same; only the per-file evidence differs.
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    let aside = f.set_aside(&at.join("1/a.lrprev"));
    fs::write(at.join("1/a.lrprev"), b"AgHg prev 9").unwrap();
    let t = purge(&f.db, 0).unwrap();
    f.assert_kept(id, &t);
    assert_eq!(fs::read(at.join("1/a.lrprev")).unwrap(), b"AgHg prev 9");
    assert_eq!(fs::read(aside).unwrap(), b"AgHg prev 1");
    assert!(at.join("root.lrprev").exists() && at.join("1/2/b.lrprev").exists());
}

/// Whatever a bundle holds now — an entry gone, a folder added, or just as
/// it moved — it is kept.
#[test]
fn a_bundle_is_kept_whether_it_lost_gained_or_kept_its_entries() {
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    let aside = f.set_aside(&at.join("1/2/b.lrprev"));
    let t = purge(&f.db, 0).unwrap();
    f.assert_kept(id, &t);
    assert!(at.join("root.lrprev").exists() && at.join("1/a.lrprev").exists());
    fs::rename(&aside, at.join("1/2/b.lrprev")).unwrap();

    fs::create_dir(at.join("1/new")).unwrap();
    let t = purge(&f.db, 0).unwrap();
    assert_eq!((t.bundles, t.files, t.bytes), (0, 0, 0), "{:?}", t.kept);
    assert!(at.join("1/new").is_dir() && at.join("1/2/b.lrprev").exists());

    fs::remove_dir(at.join("1/new")).unwrap();
    let t = purge(&f.db, 0).unwrap();
    assert_eq!((t.bundles, t.files, t.bytes), (0, 0, 0), "{:?}", t.kept);
    assert!(at.join("1/2/b.lrprev").exists());
}

/// A bundle row with no list of what moved (an older version's) is kept
/// as well, named by the entry's own path and size.
#[test]
fn a_bundle_row_without_a_list_is_kept_and_named() {
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    f.db.conn
        .execute("UPDATE journal SET manifest=NULL WHERE id=?1", [id])
        .unwrap();
    let t = purge(&f.db, 0).unwrap();
    f.assert_kept(id, &t);
    assert!(at.join("1/2/b.lrprev").exists());
}

#[test]
fn a_bundle_that_could_not_be_listed_when_it_moved_is_not_deleted() {
    // A link inside at the time of the move: the move goes ahead (one
    // rename, undone by one), the list says why it is incomplete, and purge
    // refuses — even after the link is gone.
    let f = fx();
    let dir = f.archive.join("Cat Previews.lrdata");
    fs::create_dir_all(&dir).unwrap();
    fs::write(f.root.join("target"), b"elsewhere").unwrap();
    symlink(f.root.join("target"), dir.join("link")).unwrap();
    let (id, at) = f.bundle_of(
        "Cat Previews.lrdata",
        pc_core::DerivedKind::LrPreviews,
        &[("root.lrprev", preview("0"))],
    );
    fs::remove_file(at.join("link")).unwrap();
    let t = purge(&f.db, 0).unwrap();
    f.assert_kept(id, &t);
    assert!(at.join("root.lrprev").exists());
    assert_eq!(fs::read(f.root.join("target")).unwrap(), b"elsewhere");
}

/// el-wffu8 B1-R2b: junk that is one file has no format a few bytes
/// prove, so it is never purged — not a binary `desktop.ini`, not bytes
/// that start the way `.DS_Store` or `Thumbs.db` start, not a photograph
/// or AppleDouble sidecar under such a name. Each stays, with a refusal.
#[test]
fn junk_that_is_one_file_is_never_purged_whatever_its_bytes() {
    let arbitrary: Vec<u8> = (0..=255).collect();
    for (name, bytes) in [
        (".DS_Store", b"\0\0\0\x01Bud1 window positions".to_vec()),
        (".DS_Store", arbitrary.clone()),
        (
            "Thumbs.db",
            b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1 thumbnails".to_vec(),
        ),
        ("Thumbs.db", jpeg(10)),
        (
            "desktop.ini",
            b"[.ShellClassInfo]\r\nIconResource=x\r\n".to_vec(),
        ),
        ("desktop.ini", arbitrary.clone()),
        ("._frame.jpg", b"\0\x05\x16\x07".to_vec()),
    ] {
        let f = fx();
        let (id, at) = f.junk_file(name, &bytes);
        let t = purge(&f.db, 0).unwrap();
        f.assert_kept(id, &t);
        assert_eq!(fs::read(&at).unwrap(), bytes, "{name}");
    }
}

/// A preview bundle Finder opened holds a `.DS_Store`; that file has no
/// signature that proves what it is, so the whole bundle stays.
#[test]
fn a_ds_store_inside_a_preview_bundle_keeps_the_whole_bundle() {
    let f = fx();
    let (id, at) = f.bundle_of(
        "Cat Previews.lrdata",
        pc_core::DerivedKind::LrPreviews,
        &[
            ("root.lrprev", preview("0")),
            (".DS_Store", b"\0\0\0\x01Bud1 window positions".to_vec()),
        ],
    );
    let t = purge(&f.db, 0).unwrap();
    f.assert_kept(id, &t);
    assert!(at.join("root.lrprev").exists());
    assert!(at.join(".DS_Store").exists());
}

// --- el-8s63g B2: an error after deleting keeps what was deleted. ---

#[test]
fn an_index_failure_after_purge_keeps_the_counts_and_is_journaled() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    f.db.conn
        .execute_batch(
            "CREATE TRIGGER fail_index BEFORE UPDATE OF state ON files \
             BEGIN SELECT RAISE(FAIL, 'synthetic index failure'); END;",
        )
        .unwrap();
    let t = purge(&f.db, 0).unwrap();
    assert!(!f.q.join("frame.jpg").exists());
    assert_eq!((t.bundles, t.files, t.bytes), (1, 1, 5), "{:?}", t.stopped);
    assert!(t.skipped.is_empty(), "{:?}", t.skipped);
    assert_eq!(t.stopped.len(), 1);
    assert!(
        t.stopped[0].contains("synthetic index failure"),
        "{:?}",
        t.stopped
    );
    assert_eq!(f.status(id), JournalStatus::Purged);
    assert!(f
        .events(id)
        .contains(&("purge".into(), "unfinished".into())));
}

#[test]
fn a_journal_failure_after_purge_is_a_typed_outcome_with_what_went() {
    let f = fx();
    let id = f.quarantined("frame.jpg", b"moved");
    f.db.conn
        .execute_batch(
            "CREATE TRIGGER fail_purge_close BEFORE UPDATE OF status ON journal \
             WHEN NEW.status='purged' BEGIN SELECT RAISE(FAIL, 'synthetic journal failure'); END;",
        )
        .unwrap();
    let err = purge_entry(&f.db, id).unwrap_err();
    let stop = purge_stopped(&err).expect("a typed destructive outcome");
    assert_eq!(stop.stage, PurgeStage::Recording);
    assert_eq!((stop.done.purged_files, stop.done.purged_bytes), (1, 5));
    assert!(!f.q.join("frame.jpg").exists());
    assert_eq!(f.status(id), JournalStatus::Pending);
    assert!(f
        .events(id)
        .contains(&("purge".into(), "unfinished".into())));
}

/// R2 (el-wffu8), re-mapped: a bundle is no longer taken apart against a
/// per-file list, so there is no per-entry hash to honour; what remains of
/// the boundary is that no file of it is unlinked — each still the same
/// inode with the same bytes.
#[test]
fn reviewer_r2_bundle_recorded_hash_mismatch_refuses_whole_tree() {
    use std::os::unix::fs::MetadataExt;
    let f = fx();
    let (id, at) = f.bundle("Cat Previews.lrdata");
    let rels = ["root.lrprev", "1/a.lrprev", "1/2/b.lrprev"];
    let held = || -> Vec<_> {
        rels.iter()
            .map(|r| {
                (
                    fs::metadata(at.join(r)).unwrap().ino(),
                    fs::read(at.join(r)).unwrap(),
                )
            })
            .collect()
    };
    let before = held();
    let result = purge(&f.db, 0).unwrap();
    assert_eq!(before, held(), "{result:?}");
    f.assert_kept(id, &result);
}

/// R2 independent: the desktop.ini exception must not admit arbitrary
/// unknown binary payload solely because it has a system-junk filename.
#[test]
fn reviewer_r2_binary_named_desktop_ini_must_survive() {
    use std::os::unix::fs::MetadataExt;
    let f = fx();
    let payload: Vec<u8> = (0..=255).collect();
    let (id, path) = f.junk_file("desktop.ini", &payload);
    let before = fs::metadata(&path).unwrap();
    let result = purge(&f.db, 0).unwrap();
    assert!(
        path.exists(),
        "unknown binary admitted by filename alone: {result:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), payload);
    let after = fs::metadata(&path).unwrap();
    assert_eq!(
        (before.ino(), before.mode(), before.uid(), before.gid()),
        (after.ino(), after.mode(), after.uid(), after.gid())
    );
    f.assert_kept(id, &result);
}

// --- el-3s9kp, the user's decision of 2026-10-06: nothing of Lightroom's
// is ever deleted by purge, not even one file whose every recorded field,
// content hash included, matches. ---

/// BLAKE3 of `b"moved"`, worked out once (as in
/// `a_hash_on_record_decides_over_matching_metadata`).
const OF_MOVED: &str = "51ee91ced7437f101da3822e401156f52652229e1c8ae48ab5d7b22c0764393b";

#[test]
fn nothing_of_lightrooms_is_deleted_even_as_one_proven_file() {
    for name in [
        "Cat.lrcat",
        "Cat.lrcat-wal",
        "Cat.lrcat-data/masks/mask.bin",
        "Cat Previews.lrdata/1/A/frame.lrprev",
        "Cat Smart Previews.lrdata/A/frame.dng",
        "Cat Helper.lrdata/helper.db",
        "Backups/2026-10-01 1200/Cat.lrcat.zip",
        "loose.lrprev",
        "Мой КАТАЛОГ.LRCAT",
    ] {
        let f = fx();
        let id = f.quarantined(name, b"moved");
        f.db.conn
            .execute(
                "UPDATE journal SET manifest=json_set(manifest, '$[0].proof.blake3', ?1) WHERE id=?2",
                rusqlite::params![OF_MOVED, id],
            )
            .unwrap();
        // Its evidence matches in full, hash re-read: purge could delete it.
        let e = f.db.journal_entry(id).unwrap().unwrap();
        let at = PathBuf::from(e.dst.clone().unwrap());
        let proof = e.manifest[0].proof.clone().unwrap();
        assert_eq!(proof.blake3.as_deref(), Some(OF_MOVED), "{name}");
        assert_eq!(
            proof.check_file(&fs::File::open(&at).unwrap()),
            pc_core::proof::Verdict::Same,
            "{name}"
        );

        let t = purge(&f.db, 0).unwrap();

        f.assert_kept(id, &t);
        assert!(t.kept[0].contains("Lightroom"), "{name}: {:?}", t.kept);
        assert_eq!(fs::read(&at).unwrap(), b"moved", "{name}");
    }
}

/// A frame whose companion — or whose recorded place — is Lightroom's
/// keeps the whole entry: no part of a unit goes without the rest.
#[test]
fn a_lightroom_path_anywhere_in_the_entry_keeps_all_of_it() {
    let f = fx();
    fs::write(f.archive.join("frame.xmp"), b"edits").unwrap();
    let id = f.quarantined("frame.jpg", b"moved");
    // Recorded as having come out of a catalogue's data folder.
    f.db.conn
        .execute(
            "UPDATE journal SET manifest=json_set(manifest, '$[1].src', ?1) WHERE id=?2",
            rusqlite::params![
                f.archive
                    .join("Cat.lrcat-data/frame.xmp")
                    .display()
                    .to_string(),
                id
            ],
        )
        .unwrap();
    let t = purge(&f.db, 0).unwrap();
    f.assert_kept(id, &t);
    assert_eq!(fs::read(f.q.join("frame.jpg")).unwrap(), b"moved");
    assert_eq!(fs::read(f.q.join("frame.xmp")).unwrap(), b"edits");
}

#[test]
fn lightroom_is_known_by_any_part_of_a_path_and_nothing_else_is() {
    for yes in [
        "/a/Cat.lrcat",
        "/a/Cat.lrcat-data/x",
        "/a/Cat.LRCAT-SHM",
        "/a/Cat Previews.lrdata/1/2/x.lrprev",
        "/a/Cat Smart Previews.lrdata/A/x.dng",
        "/a/Cat Helper.lrdata",
        "/a/Backups/2026-10-01 1200/Cat.lrcat.zip",
        "/a/Lightroom CC.lrlibrary/x.jpg",
        "/a/Каталог Previews.lrdata/x",
    ] {
        assert!(is_lightroom(yes), "{yes}");
    }
    for no in [
        "/a/frame.jpg",
        "/a/Lightroom/2015/frame.arw",
        "/a/.photo-cleanup-quarantine/frame.xmp",
        "/a/lrcat/frame.jpg",
        "/a/.DS_Store",
    ] {
        assert!(!is_lightroom(no), "{no}");
    }
}
