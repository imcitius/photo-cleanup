//! The namespace class of D8, cell by cell (el-3wizg; diagnosis el-lvtmk
//! §2 and §5).
//!
//! Each test stages one interleaving of another program of the same user at
//! a seam of the shared move, on a real consumer — apply, undo, reconcile,
//! organize — and then asks one judge: the object, found on disk by device
//! and inode, is where the typed outcome *and* the journal say it is,
//! verified; or the cell is a residual the test names and pins down. For
//! every relocation that leaves an object in the operation's keeping, a
//! follow-up preview does not say Gone and an undo brings it back by its
//! recorded evidence.

use super::independent_d8::signature;
use super::*;
use crate::{Placed, Role, Whereabouts};
use std::os::unix::fs::MetadataExt;

type Ident = (u64, u64);

fn ident(p: &Path) -> Ident {
    let m = fs::symlink_metadata(p).unwrap();
    (m.dev(), m.ino())
}

/// Rename `dir` aside to `to` and make an empty folder under its old name —
/// what a sync client or Finder does. Nothing is removed.
fn relocate(dir: &Path, to: &Path) {
    fs::rename(dir, to).unwrap();
    fs::create_dir(dir).unwrap();
}

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Every path under the test's folder that bears `obj`; exactly one.
fn find(a: &Archive, obj: Ident) -> PathBuf {
    let mut out = Vec::new();
    let mut stack = vec![canon(a._tmp.path())];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let m = fs::symlink_metadata(&p).unwrap();
            if (m.dev(), m.ino()) == obj {
                out.push(p.clone());
            }
            if m.file_type().is_dir() {
                stack.push(p);
            }
        }
    }
    assert_eq!(out.len(), 1, "object {obj:?} found at {out:?}");
    out.pop().unwrap()
}

fn last_id(db: &Db) -> i64 {
    db.conn
        .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
        .unwrap()
}

fn note_of(db: &Db, id: i64) -> String {
    db.conn
        .query_row(
            "SELECT coalesce(note, '') FROM journal WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

/// What a stopped run said and where it placed everything, typed.
fn stopped(e: &anyhow::Error) -> (Vec<Placed>, String) {
    let placed = crate::stopped_run(e)
        .map(|s| s.placed.clone())
        .unwrap_or_default();
    (placed, format!("{e:#}"))
}

fn told(r: &crate::ApplyReport) -> String {
    r.refused
        .iter()
        .map(|(p, w)| format!("{p}: {w}"))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// The judge of el-lvtmk §5. The object `obj`, found on disk, is the place
/// one typed [`Placed`] names as verified; the words the caller got and the
/// journal note of entry `id` name the same path; the journal's `located`
/// record says so too. Nothing is ever said to be "not touched" or "put
/// straight back". Returns the typed statement.
fn judge(a: &Archive, obj: Ident, placed: &[Placed], said: &str, id: i64) -> Placed {
    let found = find(a, obj);
    let p = placed
        .iter()
        .find(|p| p.at.at().is_some_and(|w| canon(w) == found))
        .unwrap_or_else(|| panic!("no verified statement of {found:?}: {placed:#?}"))
        .clone();
    let at = p.at.at().unwrap().display().to_string();
    assert!(said.contains(&at), "the caller was not told {at}: {said}");
    let note = note_of(&a.db, id);
    assert!(note.contains(&at), "the journal does not say {at}: {note}");
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    assert!(
        entry
            .located
            .iter()
            .any(|l| l.at.at().is_some_and(|w| canon(w) == found)),
        "no located record of {found:?}: {:#?}",
        entry.located
    );
    for words in [said, note.as_str()] {
        for banned in [
            "not touched",
            "не тронут",
            "straight back",
            "сразу возвращён",
        ] {
            assert!(
                !words.contains(banned),
                "unproven claim «{banned}»: {words}"
            );
        }
    }
    p
}

fn at_moment(
    which: race::Syscall,
    src: PathBuf,
    mut f: impl FnMut(&Path) + 'static,
) -> race::SyscallGuard {
    let mut fired = false;
    race::at_rename(move |m, s, d| {
        if m == which && s == src && !fired {
            fired = true;
            f(d);
        }
        Ok(())
    })
}

fn setup() -> (Archive, PathBuf, Ident, independent_d8::Signature) {
    let a = archive();
    let photo = a.dir.join("photo.bmp");
    bmp(&photo, [31, 71, 151]);
    let id = ident(&photo);
    let sig = signature(&photo);
    (a, photo, id, sig)
}

fn moved_aside(a: &Archive) -> PathBuf {
    a._tmp.path().join("archive-moved")
}

fn standing_of(entry: &pc_db::JournalEntry, src: &Path) -> crate::Standing {
    crate::undo_preview(entry)
        .unwrap()
        .into_iter()
        .find(|i| i.src == src.display().to_string())
        .unwrap()
        .standing
}

// ---- #1–#3: the source folder moves around the rename ------------------

#[test]
fn source_folder_relocated_before_verify_moves_nothing_and_names_its_folder() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let (root, to, p) = (a.dir.clone(), moved_aside(&a), photo.clone());
    let mut fired = false;
    let _g = race::before_move(move |s, _| {
        if s == p && !fired {
            fired = true;
            relocate(&root, &to);
        }
        Ok(())
    });
    let r = crate::apply(&a.db, a.run, &[c], None).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    assert!(r.stopped.is_some(), "a moved folder of the run stops it");
    let p = judge(&a, obj, &r.placed, &told(&r), last_id(&a.db));
    assert_eq!((p.role, p.held), (Role::Checked, false));
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("photo.bmp")));
    assert_eq!(
        signature(&find(&a, obj)),
        sig,
        "the photograph is not intact"
    );
    assert_eq!(last_row(&a.db).0, JournalStatus::Failed.as_str());
}

#[test]
fn source_folder_relocated_before_rename_default_quarantine_reports_verified_place() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let (root, to) = (a.dir.clone(), moved_aside(&a));
    let _g = at_moment(race::Syscall::Before, photo.clone(), move |_| {
        relocate(&root, &to)
    });
    let r = crate::apply(&a.db, a.run, &[c], None).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    assert!(r.stopped.is_some());
    let id = last_id(&a.db);
    let p = judge(&a, obj, &r.placed, &told(&r), id);
    assert_eq!((p.role, p.held), (Role::Checked, false));
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("photo.bmp")));
    assert_eq!(signature(&find(&a, obj)), sig);
    // Back in its own folder: nothing of the operation's to recover.
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    assert!(!crate::undo_offered(&entry));
}

#[test]
fn source_folder_relocated_before_rename_configured_quarantine_does_not_record_a_stale_origin() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let (root, to) = (a.dir.clone(), moved_aside(&a));
    let _g = at_moment(race::Syscall::Before, photo.clone(), move |_| {
        relocate(&root, &to)
    });
    let r = crate::apply(&a.db, a.run, &[c], Some(&q)).unwrap();
    drop(_g);

    // The arrival was verified, but the path it came from no longer leads
    // to its folder: not recorded as a move from `archive/photo.bmp`.
    assert_eq!(r.done.frames, 0);
    assert!(a.db.journal_quarantined(None).unwrap().is_empty());
    let id = last_id(&a.db);
    judge(&a, obj, &r.placed, &told(&r), id);
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("photo.bmp")));
    // Undo never carries it into the folder now bearing the old name.
    assert!(crate::undo(&a.db, id).is_err());
    assert!(tree(&a.dir).is_empty(), "{:?}", tree(&a.dir));
    assert_eq!(signature(&find(&a, obj)), sig);
}

// ---- #5–#7: the destination folder moves --------------------------------

#[test]
fn configured_quarantine_relocated_after_rename_returns_home_verified() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let (qq, to) = (q.clone(), a._tmp.path().join("gathered-moved"));
    let _g = at_moment(race::Syscall::After, photo.clone(), move |_| {
        relocate(&qq, &to)
    });
    let r = crate::apply(&a.db, a.run, &[c], Some(&q)).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    let p = judge(&a, obj, &r.placed, &told(&r), last_id(&a.db));
    assert!(p.at.is_verified_at(&photo), "{p:?}");
    assert_eq!(signature(&photo), sig);
    assert!(r.stopped.is_some());
}

#[test]
fn source_and_configured_quarantine_relocated_after_rename_report_verified_place() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let (root, to, qq, qto) = (
        a.dir.clone(),
        moved_aside(&a),
        q.clone(),
        a._tmp.path().join("gathered-moved"),
    );
    let _g = at_moment(race::Syscall::After, photo.clone(), move |_| {
        relocate(&root, &to);
        relocate(&qq, &qto);
    });
    let r = crate::apply(&a.db, a.run, &[c], Some(&q)).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    judge(&a, obj, &r.placed, &told(&r), last_id(&a.db));
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("photo.bmp")));
    assert_eq!(signature(&find(&a, obj)), sig);
}

#[test]
fn destination_relocated_between_holds_and_rename_is_returned_and_located() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let (qq, to) = (q.clone(), a._tmp.path().join("gathered-moved"));
    // Inside the `Before` window: after the destination was last asked,
    // before `renameat`.
    let _g = at_moment(race::Syscall::Before, photo.clone(), move |_| {
        relocate(&qq, &to)
    });
    let r = crate::apply(&a.db, a.run, &[c], Some(&q)).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    let p = judge(&a, obj, &r.placed, &told(&r), last_id(&a.db));
    assert!(p.at.is_verified_at(&photo), "{p:?}");
    assert_eq!(signature(&photo), sig);
    assert!(fs::read_dir(a._tmp.path().join("gathered-moved"))
        .unwrap()
        .all(|e| e.unwrap().file_type().unwrap().is_dir()));
}

// ---- #8, #9, #13: whose object moved -------------------------------------

#[test]
fn stranger_returned_into_relocated_source_is_located_not_put_straight_back() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let aside = a._tmp.path().join("checked-aside.bmp");
    let (root, to, p, sv) = (a.dir.clone(), moved_aside(&a), photo.clone(), aside.clone());
    let stranger = std::rc::Rc::new(std::cell::Cell::new((0u64, 0u64)));
    let st = stranger.clone();
    let _g = race::at_rename(move |m, s, _| {
        if s == p {
            match m {
                race::Syscall::Before => {
                    substitute(&p, &sv);
                    st.set(ident(&p));
                }
                race::Syscall::After => relocate(&root, &to),
                _ => {}
            }
        }
        Ok(())
    });
    let r = crate::apply(&a.db, a.run, &[c], Some(&q)).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    let id = last_id(&a.db);
    let s = judge(&a, stranger.get(), &r.placed, &told(&r), id);
    assert_eq!(s.role, Role::Stranger);
    assert_eq!(
        find(&a, stranger.get()),
        canon(&moved_aside(&a).join("photo.bmp"))
    );
    // The checked photograph is named where it is, through the file held —
    // never "not touched".
    let checked = judge(&a, obj, &r.placed, &told(&r), id);
    assert_eq!((checked.role, checked.held), (Role::Checked, false));
    assert_eq!(find(&a, obj), canon(&aside));
    assert_eq!(signature(&aside), sig);
}

#[test]
fn rewritten_arrival_returned_into_relocated_source_is_located() {
    let (a, photo, obj, _) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let (root, to) = (a.dir.clone(), moved_aside(&a));
    let _g = at_moment(race::Syscall::After, photo.clone(), move |d| {
        let mut f = fs::OpenOptions::new().append(true).open(d).unwrap();
        std::io::Write::write_all(&mut f, b"appended by a sync client").unwrap();
        relocate(&root, &to);
    });
    let r = crate::apply(&a.db, a.run, &[c], Some(&q)).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    let p = judge(&a, obj, &r.placed, &told(&r), last_id(&a.db));
    assert!(matches!(p.role, Role::Changed { .. }), "{p:?}");
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("photo.bmp")));
}

#[test]
fn arrival_taken_by_another_program_is_never_reported_untouched() {
    // Moved away, and a stranger put at its place in quarantine.
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let taken = a._tmp.path().join("taken-by-sync.bmp");
    let tk = taken.clone();
    let _g = at_moment(race::Syscall::After, photo.clone(), move |d| {
        fs::rename(d, &tk).unwrap();
        fs::write(d, STRANGER).unwrap();
    });
    let r = crate::apply(&a.db, a.run, &[c], Some(&q)).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    let id = last_id(&a.db);
    let checked = judge(&a, obj, &r.placed, &told(&r), id);
    assert_eq!(checked.role, Role::Checked);
    assert_eq!(find(&a, obj), canon(&taken));
    assert_eq!(signature(&taken), sig);
    // The stranger went to the name it was taken from, and is said so.
    assert_eq!(fs::read(&photo).unwrap(), STRANGER);
    let s = judge(&a, ident(&photo), &r.placed, &told(&r), id);
    assert_eq!(s.role, Role::Stranger);
}

#[test]
fn arrival_removed_by_another_program_is_reported_unlinked() {
    let (a, photo, _, _) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    // Another program removes the arrival and writes its own file there.
    // A disposable synthetic fixture; this tool removes nothing.
    let _g = at_moment(race::Syscall::After, photo.clone(), move |d| {
        fs::remove_file(d).unwrap();
        fs::write(d, STRANGER).unwrap();
    });
    let r = crate::apply(&a.db, a.run, &[c], Some(&q)).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    let checked = r
        .placed
        .iter()
        .find(|p| p.role == Role::Checked)
        .expect("the checked file is accounted for");
    assert_eq!(checked.at, Whereabouts::Unlinked, "{checked:?}");
    let said = told(&r);
    assert!(!said.contains("not touched"), "{said}");
    let entry = a.db.journal_entry(last_id(&a.db)).unwrap().unwrap();
    assert!(entry
        .located
        .iter()
        .any(|l| l.role == "checked" && l.at == Whereabouts::Unlinked));
}

// ---- #10, #11: a photograph kept where the return could not put it ------

#[test]
fn photo_retained_in_relocated_quarantine_is_restorable_by_proof() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a.dir.join(pc_core::QUARANTINE_DIR);
    let (qq, to, p) = (q.clone(), a.dir.join("q-parked"), photo.clone());
    let _g = at_moment(race::Syscall::After, photo.clone(), move |_| {
        fs::rename(&qq, &to).unwrap();
        fs::write(&p, b"new file at the old name").unwrap();
    });
    // Kept by the operation: the run stops and the row stays open
    // (user decision (c), point 2).
    let e = crate::apply(&a.db, a.run, &[c], None).unwrap_err();
    drop(_g);

    let (placed, said) = stopped(&e);
    assert_eq!(crate::stopped_run(&e).unwrap().done.frames, 0);
    let id = last_id(&a.db);
    assert_eq!(crate::stopped_run(&e).unwrap().pending, vec![id]);
    let held = judge(&a, obj, &placed, &said, id);
    assert!(held.held, "kept by the operation: {held:?}");
    let parked = a.dir.join("q-parked").join("photo.bmp");
    assert_eq!(find(&a, obj), canon(&parked));

    // An operation, not advice: the reconciliation finds it by its
    // evidence where the journal last proved it, and will not choose while
    // the home name is taken.
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    assert_eq!(entry.status, JournalStatus::Pending);
    assert!(!crate::undo_offered(&entry));
    let item = |db: &Db| {
        crate::reconcile(db, id)
            .unwrap()
            .into_iter()
            .find(|i| i.src == photo.display().to_string())
            .unwrap()
            .standing
    };
    assert_eq!(item(&a.db), crate::Standing::Both);
    assert!(crate::reconcile_undo(&a.db, id).is_err());
    assert_eq!(signature(&parked), sig);
    // The user settles the name; the reconciliation brings it home.
    fs::rename(&photo, a.dir.join("their-new-file.bmp")).unwrap();
    assert_eq!(item(&a.db), crate::Standing::Moved);
    crate::reconcile_undo(&a.db, id).unwrap();
    assert_eq!(signature(&photo), sig);
    assert_eq!(
        a.db.journal_entry(id).unwrap().unwrap().status,
        JournalStatus::Undone
    );
}

#[test]
fn retained_location_is_sampled_after_the_return_attempt() {
    let (a, photo, obj, _) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a.dir.join(pc_core::QUARANTINE_DIR);
    let dst = q.join("photo.bmp");
    let moved_q = a.dir.join("q-moved-after-return");
    let (p, qq, to) = (photo.clone(), q.clone(), moved_q.clone());
    let _g = race::at_rename(move |m, s, d| {
        if s == p {
            match m {
                race::Syscall::After => {
                    let mut f = fs::OpenOptions::new().append(true).open(d).unwrap();
                    std::io::Write::write_all(&mut f, b"rewritten").unwrap();
                    fs::write(&p, b"home taken").unwrap();
                }
                race::Syscall::AfterReturn => fs::rename(&qq, &to).unwrap(),
                _ => {}
            }
        }
        Ok(())
    });
    let e = crate::apply(&a.db, a.run, &[c], None).unwrap_err();
    drop(_g);

    let (placed, said) = stopped(&e);
    let p = judge(&a, obj, &placed, &said, last_id(&a.db));
    assert!(p.held && matches!(p.role, Role::Changed { .. }), "{p:?}");
    assert_eq!(find(&a, obj), canon(&moved_q.join("photo.bmp")));
    let stale = format!("{}", dst.display());
    assert!(
        !p.at.at().unwrap().display().to_string().contains(&stale),
        "a place sampled before the return: {p:?}"
    );
}

// ---- #12, #14–#16: nothing moved; interruptions ---------------------------

#[test]
fn stranger_at_destination_before_rename_moves_nothing() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let _g = at_moment(race::Syscall::Before, photo.clone(), move |d| {
        fs::write(d, STRANGER).unwrap()
    });
    let r = crate::apply(&a.db, a.run, &[c], None).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    assert_eq!(find(&a, obj), canon(&photo));
    assert_eq!(signature(&photo), sig);
    let q = a.dir.join(pc_core::QUARANTINE_DIR).join("photo.bmp");
    assert_eq!(fs::read(&q).unwrap(), STRANGER, "the stranger was replaced");
    assert_eq!(last_row(&a.db).0, JournalStatus::Failed.as_str());
}

fn interrupted(a: &Archive, q: Option<&Path>, c: Candidate) -> i64 {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::apply(&a.db, a.run, &[c], q)
    }));
    assert!(r.is_err(), "the interruption did not happen");
    let id = last_id(&a.db);
    assert_eq!(
        a.db.journal_entry(id).unwrap().unwrap().status,
        JournalStatus::Pending
    );
    id
}

#[test]
fn crash_after_rename_reconciles_by_proof() {
    let (a, photo, _, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let _g = at_moment(race::Syscall::After, photo.clone(), |_| {
        panic!("simulated interruption")
    });
    let id = interrupted(&a, None, c);
    drop(_g);

    crate::reconcile_undo(&a.db, id).unwrap();
    assert_eq!(signature(&photo), sig);
    assert_eq!(
        a.db.journal_entry(id).unwrap().unwrap().status,
        JournalStatus::Undone
    );
}

/// RESIDUAL (D1, follow-up el-6byp2): with its quarantine folder moved as
/// well, an interrupted move is found by no path the journal holds, and no
/// search is made. The entry stays pending, says why, and nothing moves.
#[test]
fn crash_after_rename_with_relocated_quarantine_is_found_by_proof_or_stays_pending_with_reason() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a.dir.join(pc_core::QUARANTINE_DIR);
    let (qq, to) = (q.clone(), a.dir.join("q-parked"));
    let _g = at_moment(race::Syscall::After, photo.clone(), move |_| {
        fs::rename(&qq, &to).unwrap();
        panic!("simulated interruption");
    });
    let id = interrupted(&a, None, c);
    drop(_g);

    let e = crate::reconcile_undo(&a.db, id).unwrap_err();
    let why = format!("{e:#}");
    assert!(why.contains(&photo.display().to_string()), "{why}");
    assert_eq!(
        a.db.journal_entry(id).unwrap().unwrap().status,
        JournalStatus::Pending
    );
    let parked = a.dir.join("q-parked").join("photo.bmp");
    assert_eq!(find(&a, obj), canon(&parked));
    assert_eq!(signature(&parked), sig);
}

#[test]
fn crash_after_compensating_return_is_reconciled_by_proof() {
    let (a, photo, _, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let (qq, to, p) = (
        q.clone(),
        a._tmp.path().join("gathered-moved"),
        photo.clone(),
    );
    let _g = race::at_rename(move |m, s, _| {
        if s == p {
            match m {
                race::Syscall::After => relocate(&qq, &to),
                race::Syscall::AfterReturn => panic!("simulated interruption"),
                _ => {}
            }
        }
        Ok(())
    });
    let id = interrupted(&a, Some(&q), c);
    drop(_g);

    // Returned home before the interruption: found there by its evidence.
    let items = crate::reconcile(&a.db, id).unwrap();
    assert_eq!(items[0].standing, crate::Standing::Home);
    crate::reconcile_undo(&a.db, id).unwrap();
    assert_eq!(signature(&photo), sig);
}

/// RESIDUAL (D1): the same interruption when the return went into a source
/// folder that had been moved: no path of the journal leads there. Pending,
/// with the reason; nothing moves.
#[test]
fn crash_after_compensating_return_into_relocated_source_stays_pending_with_reason() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let q = a._tmp.path().join("gathered");
    let (root, to, p) = (a.dir.clone(), moved_aside(&a), photo.clone());
    let _g = race::at_rename(move |m, s, _| {
        if s == p {
            match m {
                race::Syscall::After => relocate(&root, &to),
                race::Syscall::AfterReturn => panic!("simulated interruption"),
                _ => {}
            }
        }
        Ok(())
    });
    let id = interrupted(&a, Some(&q), c);
    drop(_g);

    assert!(crate::reconcile_undo(&a.db, id).is_err());
    assert_eq!(
        a.db.journal_entry(id).unwrap().unwrap().status,
        JournalStatus::Pending
    );
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("photo.bmp")));
    assert_eq!(signature(&find(&a, obj)), sig);
}

// ---- #17: what is recorded is asked again after the last rename ---------

/// A photograph moved and verified; its sidecar's move meets the archive
/// folder being moved. Returns the archive, the journal entry, and the
/// photograph's identity and signature.
fn frame_then_sidecar_meets_a_moved_folder(
    a: &Archive,
) -> (crate::ApplyReport, i64, Ident, independent_d8::Signature) {
    let photo = a.dir.join("photo.bmp");
    bmp(&photo, [31, 71, 151]);
    let (obj, sig) = (ident(&photo), signature(&photo));
    let xmp = a.dir.join("photo.xmp");
    fs::write(&xmp, b"<x:xmpmeta/>").unwrap();
    let c = manual(&a.db, a.run, &photo);
    let (root, to) = (a.dir.clone(), moved_aside(a));
    let _g = at_moment(race::Syscall::Before, xmp, move |_| relocate(&root, &to));
    let r = crate::apply(&a.db, a.run, &[c], None).unwrap();
    (r, last_id(&a.db), obj, sig)
}

#[test]
fn frame_location_is_reverified_after_companion_moves_before_the_journal_is_finalised() {
    let a = archive();
    let photo = a.dir.join("photo.bmp");
    let (r, id, obj, sig) = frame_then_sidecar_meets_a_moved_folder(&a);

    // The sidecar could not follow: the frame does not stay moved without
    // it (user decision (c)). Both are back in their folder — which another
    // program moved, so the run stops and says where they are.
    assert_eq!((r.done.frames, r.done.companions), (0, 0));
    assert!(r.stopped.is_some());
    let frame = judge(&a, obj, &r.placed, &told(&r), id);
    assert!(!frame.held);
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("photo.bmp")));
    assert_eq!(signature(&find(&a, obj)), sig);
    let xmp = moved_aside(&a).join("photo.xmp");
    let side = judge(&a, ident(&xmp), &r.placed, &told(&r), id);
    assert!(!side.held);

    // Nothing stays in the tool's keeping: the row is refused, not done.
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    assert_eq!(entry.status, JournalStatus::Failed);
    assert!(crate::undo(&a.db, id).is_err());
    assert!(!photo.exists());
}

#[test]
fn a_folder_moved_right_before_the_record_is_recorded_where_the_photo_is() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    let (root, to) = (a.dir.clone(), moved_aside(&a));
    let _g = at_moment(race::Syscall::BeforeRecord, photo.clone(), move |_| {
        relocate(&root, &to)
    });
    let r = crate::apply(&a.db, a.run, &[c], None).unwrap();
    drop(_g);

    // Not proven where it went right before the record: the unit goes back
    // (user decision (c), point 2) — into its own folder, which moved, so
    // the run stops and says where it is.
    assert_eq!(r.done.frames, 0);
    assert!(r.stopped.is_some());
    let id = last_id(&a.db);
    let p = judge(&a, obj, &r.placed, &told(&r), id);
    assert!(!p.held);
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("photo.bmp")));
    assert_eq!(signature(&find(&a, obj)), sig);
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    assert_eq!(entry.status, JournalStatus::Failed);
    assert!(!photo.exists());
}

// ---- #18, #19: the other directions ---------------------------------------

#[test]
fn undo_whose_home_is_relocated_after_rename_records_where_the_photo_is_and_retry_restores_it() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    assert_eq!(
        crate::apply(&a.db, a.run, &[c], None).unwrap().done.frames,
        1
    );
    let id = last_id(&a.db);
    let held = a.dir.join(pc_core::QUARANTINE_DIR).join("photo.bmp");
    let (root, to) = (a.dir.clone(), moved_aside(&a));
    let _g = at_moment(race::Syscall::After, held, move |_| relocate(&root, &to));
    let e = crate::undo(&a.db, id).unwrap_err();
    drop(_g);

    assert!(crate::is_folder_moved(&e), "{e:#}");
    let placed = crate::stopped_run(&e)
        .map(|s| s.placed.clone())
        .unwrap_or_default();
    let p = judge(&a, obj, &placed, &format!("{e:#}"), id);
    assert!(p.held, "back in the operation's keeping: {p:?}");
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    assert_eq!(entry.status, JournalStatus::Done);
    assert_eq!(standing_of(&entry, &photo), crate::Standing::Moved);
    crate::undo(&a.db, id).unwrap();
    assert_eq!(signature(&photo), sig);
}

#[test]
fn organize_with_relocated_tree_reports_verified_place() {
    let a = archive();
    let src = a.dir.join("a.jpg");
    let dst = a.dir.join("2019/a.jpg");
    fs::write(&src, b"picture").unwrap();
    let obj = ident(&src);
    let mtime = pc_core::time::mtime_unix(&fs::metadata(&src).unwrap());
    let m = pc_organize::Move {
        file_id: indexed(&a.db, a.run, &src),
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        size: 7,
        mtime,
        date: pc_organize::Dated {
            ts: 1_562_000_000,
            source: pc_organize::Source::Exif,
            precision: pc_organize::Precision::Day,
        },
        event: "2019".into(),
        renamed_from: None,
    };
    let (root, to) = (a.dir.clone(), moved_aside(&a));
    let _g = at_moment(race::Syscall::After, src.clone(), move |_| {
        relocate(&root, &to)
    });
    let r = crate::organize(&a.db, a.run, &[m]).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 0);
    assert!(r.stopped.is_some());
    let said = r
        .refused
        .iter()
        .map(|(p, w)| format!("{p}: {w}"))
        .collect::<Vec<_>>()
        .join(" / ");
    judge(&a, obj, &r.placed, &said, last_id(&a.db));
    assert_eq!(find(&a, obj), canon(&moved_aside(&a).join("a.jpg")));
    assert_eq!(fs::read(find(&a, obj)).unwrap(), b"picture");
}

/// RESIDUAL (D3, follow-up el-6byp2): the archive folder — and with it the
/// quarantine beside the photographs — moved after the run finished. No
/// path of the journal leads there; undo refuses, names both paths, and
/// moves nothing.
#[test]
fn post_completion_folder_move_is_a_documented_residual_or_found_by_proof() {
    let (a, photo, obj, sig) = setup();
    let c = manual(&a.db, a.run, &photo);
    assert_eq!(
        crate::apply(&a.db, a.run, &[c], None).unwrap().done.frames,
        1
    );
    let id = last_id(&a.db);
    relocate(&a.dir, &moved_aside(&a));

    let entry = a.db.journal_entry(id).unwrap().unwrap();
    assert_eq!(standing_of(&entry, &photo), crate::Standing::Gone);
    let e = crate::undo(&a.db, id).unwrap_err();
    assert!(format!("{e:#}").contains(&photo.display().to_string()));
    assert!(tree(&a.dir).is_empty());
    let held = moved_aside(&a)
        .join(pc_core::QUARANTINE_DIR)
        .join("photo.bmp");
    assert_eq!(find(&a, obj), canon(&held));
    assert_eq!(signature(&held), sig);
}

// ---- D2, D6b ---------------------------------------------------------------

#[test]
fn a_run_root_moved_between_two_photographs_stops_the_run() {
    let a = archive();
    let other = a._tmp.path().join("other");
    let first = other.join("first.bmp");
    bmp(&first, [1, 2, 3]);
    let second = a.dir.join("second.bmp");
    bmp(&second, [4, 5, 6]);
    let sig = signature(&second);
    a.db.start_run(&[other.display().to_string()], "test")
        .unwrap();
    let cs = vec![manual(&a.db, a.run, &first), manual(&a.db, a.run, &second)];
    let (root, to) = (a.dir.clone(), moved_aside(&a));
    // After the first photograph's last rename, before its record: the
    // archive — a root of the run, not the first photograph's folder — is
    // moved by another program.
    let _g = at_moment(race::Syscall::BeforeRecord, first.clone(), move |_| {
        relocate(&root, &to)
    });
    let r = crate::apply(&a.db, a.run, &cs, None).unwrap();
    drop(_g);

    assert_eq!(r.done.frames, 1);
    assert!(r.stopped.is_some(), "{r:?}");
    let (path, why) = r.refused.last().unwrap();
    assert_eq!(path, &second.display().to_string());
    assert_eq!(why, &crate::files::not_tried_folder());
    assert_eq!(signature(&moved_aside(&a).join("second.bmp")), sig);
    assert!(r.stop_error().is_some_and(|e| crate::is_folder_moved(&e)));
}

#[test]
fn purge_refuses_rows_with_unverified_items() {
    let a = archive();
    // A done row whose undo met a moved folder: the photo was put back into
    // the tool's keeping, where the journal proved it — not at its path.
    let photo = a.dir.join("photo.bmp");
    bmp(&photo, [31, 71, 151]);
    let (obj, sig) = (ident(&photo), signature(&photo));
    let c = manual(&a.db, a.run, &photo);
    assert_eq!(
        crate::apply(&a.db, a.run, &[c], None).unwrap().done.frames,
        1
    );
    let id = last_id(&a.db);
    let held = a.dir.join(pc_core::QUARANTINE_DIR).join("photo.bmp");
    let (root, to) = (a.dir.clone(), moved_aside(&a));
    let g = at_moment(race::Syscall::After, held, move |_| relocate(&root, &to));
    assert!(crate::undo(&a.db, id).is_err());
    drop(g);
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    assert_eq!(entry.status, JournalStatus::Done);
    assert!(!entry.located.is_empty());

    assert!(crate::purge_entry(&a.db, id).is_err());
    let t = crate::purge(&a.db, 0).unwrap();
    assert_eq!(t.files, 0, "{t:?}");
    assert_eq!(t.skipped.len(), 1, "{t:?}");
    assert_eq!(signature(&find(&a, obj)), sig, "the photograph is gone");
    assert_eq!(
        a.db.journal_entry(id).unwrap().unwrap().status,
        JournalStatus::Done
    );
}
