//! A frame and its companions are one unit (user decision (c) of
//! 2026-10-05, after el-4pk1q B1-R3..B4-R3).
//!
//! Each of these was a partial state: a frame done while its sidecar sat
//! elsewhere, a refused sidecar recorded at a stale path, a reconciliation
//! half done, a changed photo offered for automatic recovery. Under the unit
//! rule none of them may exist: the unit moves whole or is put back whole;
//! if it cannot be put back whole the row stays open (pending), every
//! member's place is recorded and shown, and nothing is undone, reconciled
//! or closed until every member is proven.

use super::independent_d8::signature;
use super::*;
use std::cell::RefCell;
use std::os::unix::fs::MetadataExt;
use std::rc::Rc;

fn ident(p: &Path) -> (u64, u64) {
    let m = fs::symlink_metadata(p).unwrap();
    (m.dev(), m.ino())
}

/// A recorded place that says "verified at P" must be true: P bears the
/// object the record is about.
fn verified_claims_are_true(entry: &pc_db::JournalEntry) {
    for l in &entry.located {
        if let (Some(at), Some(p)) = (l.at.at(), l.proof.as_ref()) {
            let m = fs::symlink_metadata(at)
                .unwrap_or_else(|e| panic!("recorded as verified at {at:?}, but: {e}; {l:?}"));
            assert_eq!(
                (m.dev(), m.ino()),
                (p.dev, p.ino),
                "recorded as verified at {at:?}, but another object is there: {l:?}"
            );
        }
    }
}

/// The frame moves; during its sidecar's move the quarantine folder is
/// moved aside and a foreign payload takes the sidecar's home name, so the
/// sidecar cannot go back. With `relocate_again`, the quarantine folder
/// moves once more right before the record.
fn sidecar_cannot_go_back(
    relocate_again: bool,
) -> (
    Archive,
    PathBuf,
    PathBuf,
    PathBuf,
    anyhow::Result<crate::ApplyReport>,
) {
    let a = archive();
    let photo = a.dir.join("photo.bmp");
    let side = a.dir.join("photo.xmp");
    bmp(&photo, [31, 71, 151]);
    fs::write(&side, b"irreplaceable synthetic Lightroom edit").unwrap();
    let frame_sig = signature(&photo);
    let side_sig = signature(&side);
    let c = manual(&a.db, a.run, &photo);
    let q = a.dir.join(pc_core::QUARANTINE_DIR);
    let parked = a.dir.join("q-parked");
    let final_q = a.dir.join("q-final");
    let foreign = Rc::new(RefCell::new(None));
    let saved = foreign.clone();
    let (s, qq, pp, ff) = (side.clone(), q, parked.clone(), final_q.clone());
    let mut fired = false;
    let mut again = false;
    let _g = race::at_rename(move |moment, src, _| {
        if moment == race::Syscall::After && src == s && !fired {
            fired = true;
            fs::rename(&qq, &pp).unwrap();
            fs::write(&s, STRANGER).unwrap();
            *saved.borrow_mut() = Some(signature(&s));
        }
        if relocate_again && moment == race::Syscall::BeforeRecord && !again {
            again = true;
            fs::rename(&pp, &ff).unwrap();
        }
        Ok(())
    });
    let r = crate::apply(&a.db, a.run, &[c], None);
    drop(_g);
    let kept = if relocate_again {
        final_q.join("photo.xmp")
    } else {
        parked.join("photo.xmp")
    };
    assert_eq!(
        signature(&kept),
        side_sig,
        "sidecar payload/metadata changed"
    );
    assert_eq!(
        signature(&side),
        foreign.borrow().clone().unwrap(),
        "foreign payload/metadata changed"
    );
    // The frame was put back with the unit: home, unchanged.
    assert_eq!(signature(&photo), frame_sig, "frame not put back home");
    (a, photo, side, kept, r)
}

#[test]
fn b1_r3_a_sidecar_that_cannot_go_back_keeps_the_unit_open_and_undo_refuses_it() {
    let (a, photo, side, kept, r) = sidecar_cannot_go_back(false);
    let e = r.expect_err("a unit that could not be put back whole must stop the run");
    let stop = crate::stopped_run(&e).expect("typed stop");
    assert_eq!(stop.done.frames, 0, "a frame counted done: {e:#}");
    let entry =
        a.db.journal_pending()
            .unwrap()
            .pop()
            .expect("row left open");
    assert_eq!(stop.pending, vec![entry.id]);
    verified_claims_are_true(&entry);
    let xmp = entry
        .overlay_of(&entry.manifest[1])
        .expect("sidecar's place recorded");
    assert!(xmp.held);
    assert_eq!(
        xmp.at.at().map(|p| fs::canonicalize(p).unwrap()),
        Some(fs::canonicalize(&kept).unwrap())
    );
    // Shown to the person: both members.
    let shown: Vec<&str> = stop.placed.iter().map(|p| p.recorded.as_str()).collect();
    assert!(shown.contains(&side.to_str().unwrap()), "{shown:?}");
    assert!(shown.contains(&photo.to_str().unwrap()), "{shown:?}");

    // Undo does not close it: the row is not done.
    assert!(crate::undo(&a.db, entry.id).is_err());
    assert_eq!(
        a.db.journal_entry(entry.id).unwrap().unwrap().status,
        JournalStatus::Pending
    );
    // While the home name is taken, reconciliation refuses the whole unit.
    let foreign = signature(&side);
    assert!(crate::reconcile_undo(&a.db, entry.id).is_err());
    assert_eq!(
        a.db.journal_entry(entry.id).unwrap().unwrap().status,
        JournalStatus::Pending
    );
    assert!(kept.exists() && photo.exists());

    // Freed by the person, the whole unit comes back by proof.
    let aside = a.dir.join("foreign-kept.xmp");
    fs::rename(&side, &aside).unwrap();
    crate::reconcile_undo(&a.db, entry.id).unwrap();
    assert_eq!(signature(&aside), foreign);
    assert!(side.exists() && !kept.exists() && photo.exists());
    assert_eq!(
        fs::read(&side).unwrap(),
        b"irreplaceable synthetic Lightroom edit"
    );
    assert_eq!(
        a.db.journal_entry(entry.id).unwrap().unwrap().status,
        JournalStatus::Undone
    );
}

#[test]
fn b2_r3_no_stale_verified_place_is_recorded_for_a_sidecar_kept_by_the_unit() {
    let (a, _, side, kept, r) = sidecar_cannot_go_back(true);
    let e = r.expect_err("run stops");
    let stop = crate::stopped_run(&e).unwrap();
    let entry = a.db.journal_pending().unwrap().pop().unwrap();
    verified_claims_are_true(&entry);
    let l = entry
        .located
        .iter()
        .rev()
        .find(|l| l.src == side.display().to_string())
        .unwrap();
    assert_eq!(
        l.at.at().map(|p| fs::canonicalize(p).unwrap()),
        Some(fs::canonicalize(&kept).unwrap()),
        "journal names a stale place: {l:?}"
    );
    let p = stop
        .placed
        .iter()
        .find(|p| p.recorded == side.display().to_string())
        .unwrap();
    assert_eq!(
        p.at, l.at,
        "the person and the journal are told different places"
    );
}

#[test]
fn b3_r3_a_reconciliation_that_cannot_finish_is_put_back_whole_and_refused() {
    let a = archive();
    let photo = a.dir.join("photo.bmp");
    let side = a.dir.join("photo.xmp");
    bmp(&photo, [31, 71, 151]);
    fs::write(&side, b"irreplaceable synthetic Lightroom edit").unwrap();
    let frame = ident(&photo);
    let frame_sig = signature(&photo);
    let side_sig = signature(&side);
    let c = manual(&a.db, a.run, &photo);
    let _g = race::at_rename(|moment, _, _| {
        if moment == race::Syscall::BeforeRecord {
            panic!("fixture interruption before closing journal");
        }
        Ok(())
    });
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::apply(&a.db, a.run, &[c], None)
    }));
    drop(_g);
    assert!(interrupted.is_err());
    let entry = a.db.journal_pending().unwrap().pop().unwrap();
    let xmp_dst = entry
        .manifest
        .iter()
        .find(|m| m.src == side.display().to_string())
        .unwrap()
        .dst
        .clone();
    let relocated = a._tmp.path().join("archive-relocated");
    let (root, to) = (a.dir.clone(), relocated.clone());
    let mut fired = false;
    let _g = race::at_rename(move |moment, src, _| {
        if moment == race::Syscall::After && src == Path::new(&xmp_dst) && !fired {
            fired = true;
            fs::rename(&root, &to).unwrap();
            fs::create_dir(&root).unwrap();
        }
        Ok(())
    });
    let err = crate::reconcile_undo(&a.db, entry.id).unwrap_err();
    drop(_g);
    let shown = format!("{err:#}");
    assert!(
        !shown.contains("already back") && !shown.contains("уже вернулось"),
        "a partial reconciliation is claimed: {shown}"
    );
    // Nothing was left half back: the frame is not sitting at home while
    // its sidecar stays in quarantine.
    assert!(
        !relocated.join("photo.bmp").exists(),
        "frame left half reconciled"
    );
    assert!(!a.dir.join("photo.bmp").exists());
    let after = a.db.journal_entry(entry.id).unwrap().unwrap();
    assert_eq!(after.status, JournalStatus::Pending);
    verified_claims_are_true(&after);
    // The frame's current place, found by its identity, is what the journal
    // last recorded for it.
    let q = relocated.join(pc_core::QUARANTINE_DIR);
    let frame_now = q.join("photo.bmp");
    assert_eq!(ident(&frame_now), frame);
    assert_eq!(signature(&frame_now).7, frame_sig.7);
    assert_eq!(signature(&q.join("photo.xmp")).7, side_sig.7);
    let l = after
        .overlay_of(&after.manifest[0])
        .expect("frame place recorded");
    assert_eq!(
        l.at.at().map(|p| fs::canonicalize(p).unwrap()),
        Some(fs::canonicalize(&frame_now).unwrap())
    );
    // And it is not lost to the next look.
    for item in crate::reconcile(&a.db, entry.id).unwrap() {
        assert_ne!(item.standing, crate::Standing::Gone, "{item:?}");
    }
}

#[test]
fn b4_r3_a_changed_photo_kept_by_the_tool_is_left_for_manual_recovery() {
    let a = archive();
    let photo = a.dir.join("photo.bmp");
    bmp(&photo, [31, 71, 151]);
    let c = manual(&a.db, a.run, &photo);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let p = photo.clone();
    let _g = race::at_rename(move |moment, src, dst| {
        if moment == race::Syscall::After && src == p {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(dst)
                .unwrap()
                .write_all(b"a synthetic update")
                .unwrap();
            fs::write(&p, STRANGER).unwrap();
            *capture.borrow_mut() = Some((signature(dst), signature(&p)));
        }
        Ok(())
    });
    let r = crate::apply(&a.db, a.run, &[c], None);
    drop(_g);
    let e = r.expect_err("a photo the tool keeps stops the run");
    assert_eq!(crate::stopped_run(&e).unwrap().done.frames, 0);
    let entry =
        a.db.journal_pending()
            .unwrap()
            .pop()
            .expect("row left open");
    verified_claims_are_true(&entry);
    let l = entry
        .located
        .iter()
        .find(|l| l.held && l.role == "changed")
        .unwrap();
    let kept = l.at.at().unwrap().to_path_buf();
    let (changed, foreign) = saved.borrow().clone().unwrap();
    assert_eq!(signature(&kept), changed);
    let aside = a.dir.join("foreign-preserved.bmp");
    fs::rename(&photo, &aside).unwrap();

    // Told as needing a person, with its path; never moved by recovery.
    let items = crate::reconcile(&a.db, entry.id).unwrap();
    let why = items[0].why().expect("a changed photo is not offered");
    assert!(why.contains(&kept.display().to_string()), "{why}");
    assert!(crate::reconcile_undo(&a.db, entry.id).is_err());
    assert!(crate::undo(&a.db, entry.id).is_err());
    assert_eq!(
        a.db.journal_entry(entry.id).unwrap().unwrap().status,
        JournalStatus::Pending,
        "the row was closed"
    );
    assert!(
        !photo.exists(),
        "a changed photo was recovered automatically"
    );
    assert_eq!(signature(&kept), changed);
    assert_eq!(signature(&aside), foreign);
}

#[test]
fn an_undo_that_cannot_put_its_unit_back_keeps_the_entry_open_and_says_where_each_is() {
    let a = archive();
    let photo = a.dir.join("photo.bmp");
    let side = a.dir.join("photo.xmp");
    bmp(&photo, [31, 71, 151]);
    fs::write(&side, b"irreplaceable synthetic Lightroom edit").unwrap();
    let frame_sig = signature(&photo);
    let c = manual(&a.db, a.run, &photo);
    assert_eq!(
        crate::apply(&a.db, a.run, &[c], None).unwrap().done.frames,
        1
    );
    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();
    let q = a.dir.join(pc_core::QUARANTINE_DIR);
    let (held_frame, held_side) = (q.join("photo.bmp"), q.join("photo.xmp"));

    // The frame comes home; right before the sidecar's move, one program
    // takes the sidecar's home name and another the frame's quarantine
    // name — so the sidecar cannot come and the frame cannot go back.
    let (s, hf) = (held_side.clone(), held_frame.clone());
    let home_side = side.clone();
    let mut fired = false;
    let _g = race::at_rename(move |moment, src, _| {
        if moment == race::Syscall::Before && src == s && !fired {
            fired = true;
            fs::write(&home_side, STRANGER).unwrap();
            fs::write(&hf, b"a file another program put in quarantine").unwrap();
        }
        Ok(())
    });
    let e = crate::undo(&a.db, entry.id).unwrap_err();
    drop(_g);

    let stop = crate::stopped_run(&e).expect("the run stops");
    let after = a.db.journal_entry(entry.id).unwrap().unwrap();
    assert_eq!(
        after.status,
        JournalStatus::Done,
        "closed with a member away"
    );
    verified_claims_are_true(&after);
    // Each member is said where it is: the frame at home (not the
    // operation's), the sidecar held.
    let f = after.overlay_of(&after.manifest[0]).unwrap();
    assert!(!f.held && f.at.is_verified_at(&photo), "{f:?}");
    let x = after.overlay_of(&after.manifest[1]).unwrap();
    assert!(x.held && x.at.is_verified_at(&held_side), "{x:?}");
    assert_eq!(stop.placed.len(), 2, "{:?}", stop.placed);
    assert_eq!(signature(&photo), frame_sig);
    assert_eq!(fs::read(&side).unwrap(), STRANGER);

    // Nothing more moves while the sidecar's home is taken.
    assert!(crate::undo(&a.db, entry.id).is_err());
    assert!(held_side.exists());
    // Nor while a stranger sits at the frame's recorded quarantine name:
    // the shared classifier does not choose around it (leave extra).
    fs::rename(&side, a.dir.join("their.xmp")).unwrap();
    assert!(crate::undo(&a.db, entry.id).is_err());
    // Freed by the person, the rest of the unit comes home, and only then
    // is the entry closed.
    let theirs = a.dir.join("their-quarantined.bmp");
    fs::rename(&held_frame, &theirs).unwrap();
    crate::undo(&a.db, entry.id).unwrap();
    assert_eq!(signature(&photo), frame_sig);
    assert_eq!(
        fs::read(&side).unwrap(),
        b"irreplaceable synthetic Lightroom edit"
    );
    assert_eq!(
        fs::read(&theirs).unwrap(),
        b"a file another program put in quarantine"
    );
    assert_eq!(
        a.db.journal_entry(entry.id).unwrap().unwrap().status,
        JournalStatus::Undone
    );
}
