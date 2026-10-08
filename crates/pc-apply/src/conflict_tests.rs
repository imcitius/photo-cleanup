//! An undo whose original place is taken: each of the user's choices
//! (el-14vx0), on the real consumers — a photograph quarantined by
//! `quarantine_file` with its `.xmp`, and a run of `organize` — on
//! disposable fixtures only.

use crate::{race, Choice, Conflict, Reviewed};
use pc_db::{Db, JournalStatus};
use pc_family::plan::Candidate;
use std::fs;
use std::path::{Path, PathBuf};

const OURS: &[u8] = b"the photograph that went to quarantine";
const OUR_EDITS: &[u8] = b"<x:xmpmeta>our edits</x:xmpmeta>";
const THEIRS: &[u8] = b"a newer photograph that took its name";
const THEIR_EDITS: &[u8] = b"<x:xmpmeta>their edits</x:xmpmeta>";

struct Archive {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
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
        dir,
        _tmp: tmp,
        db,
        run,
    }
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

fn q(home: &Path) -> PathBuf {
    home.parent()
        .unwrap()
        .join(pc_core::QUARANTINE_DIR)
        .join(home.file_name().unwrap())
}

/// `IMG.CR2` with `IMG.xmp`, quarantined by the tool's own forward move;
/// the journal entry's id.
fn quarantined(a: &Archive, name: &str) -> (i64, PathBuf, PathBuf) {
    let home = a.dir.join(name);
    let xmp = home.with_extension("xmp");
    fs::write(&home, OURS).unwrap();
    fs::write(&xmp, OUR_EDITS).unwrap();
    let file_id =
        a.db.upsert_file(
            &pc_db::NewFile {
                path: s(&home),
                name: name.into(),
                size: OURS.len() as i64,
                ..Default::default()
            },
            a.run,
        )
        .unwrap();
    let c = Candidate {
        file_id,
        family_id: 0,
        path: s(&home),
        size: OURS.len() as i64,
        role: pc_family::Role::Copy,
        keeper_id: 0,
        keeper_path: String::new(),
        reason: "выбор человека".into(),
        manual: true,
        group_keeper: String::new(),
    };
    let filed = crate::files::quarantine_file(&a.db, a.run, &c, None).unwrap();
    assert_eq!(filed.outcome, crate::FileOutcome::Moved, "{}", filed.why);
    assert!(q(&home).exists() && q(&xmp).exists());
    let id =
        a.db.conn
            .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
            .unwrap();
    (id, home, xmp)
}

/// Another program puts its own photograph and sidecar at the old names.
fn taken(home: &Path, xmp: &Path) {
    fs::write(home, THEIRS).unwrap();
    fs::write(xmp, THEIR_EDITS).unwrap();
}

fn status(db: &Db, id: i64) -> JournalStatus {
    db.journal_entry(id).unwrap().unwrap().status
}

fn events(db: &Db, id: i64) -> Vec<(String, String, String)> {
    db.journal_events(id)
        .unwrap()
        .into_iter()
        .map(|e| (e.phase, e.kind, e.data.unwrap_or_default()))
        .collect()
}

fn with(choice: Choice) -> impl FnMut(&Conflict) -> anyhow::Result<Choice> {
    move |_| Ok(choice)
}

#[test]
fn a_taken_place_keeps_the_file_in_quarantine_by_default_and_says_where() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);

    let e = crate::undo(&a.db, id).unwrap_err();

    let kept = crate::conflict_kept(&e).expect("a typed kept conflict");
    assert_eq!(kept.conflict.returning[0].home, s(&home));
    assert_eq!(kept.conflict.returning[0].held, s(&q(&home)));
    let words = format!("{e:#}");
    assert!(words.contains(&s(&q(&home))), "{words}");
    assert!(words.contains(&s(&home)), "{words}");
    // Both units are where they were, byte for byte.
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
    assert_eq!(fs::read(q(&xmp)).unwrap(), OUR_EDITS);
    assert_eq!(status(&a.db, id), JournalStatus::Done);
    let ev = events(&a.db, id);
    let last = ev.last().unwrap();
    assert_eq!((last.0.as_str(), last.1.as_str()), ("undo", "kept"));
    assert!(last.2.contains("\"choice\":\"keep\""), "{}", last.2);
    // The existing frame's own sidecar is part of the existing unit.
    let occ: Vec<&str> = kept
        .conflict
        .occupants
        .iter()
        .map(|o| o.path.as_str())
        .collect();
    assert_eq!(occ, [s(&home), s(&xmp)]);
    assert_eq!(kept.conflict.choices, Choice::ALL.to_vec());
}

#[test]
fn replace_carries_the_existing_unit_into_quarantine_under_its_own_undoable_entry() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    // The existing frame's other sidecar travels with it too.
    let theirs_too = a.dir.join("IMG.CR2.xmp");
    fs::write(&theirs_too, b"their second sidecar").unwrap();

    let done = crate::undo_with(&a.db, id, &mut with(Choice::Replace)).unwrap();

    assert_eq!(fs::read(&home).unwrap(), OURS);
    assert_eq!(fs::read(&xmp).unwrap(), OUR_EDITS);
    assert!(!theirs_too.exists());
    assert_eq!(done.files_back, 2);
    assert_eq!(done.set_aside, 3);
    assert_eq!(done.entries_back, 1);
    assert_eq!(status(&a.db, id), JournalStatus::Undone);
    // Theirs is in quarantine — under free names, since ours sat at the
    // plain ones until it came back — never deleted.
    let aside: i64 =
        a.db.conn
            .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
            .unwrap();
    let j2 = a.db.journal_entry(aside).unwrap().unwrap();
    assert_eq!(j2.op, "quarantine-file");
    assert_eq!(j2.status, JournalStatus::Done);
    let parked: Vec<(String, Vec<u8>)> = j2
        .manifest
        .iter()
        .map(|m| (m.dst.clone(), fs::read(&m.dst).unwrap()))
        .collect();
    let qdir = home.parent().unwrap().join(pc_core::QUARANTINE_DIR);
    assert_eq!(
        parked,
        [
            (s(&qdir.join("IMG_1.CR2")), THEIRS.to_vec()),
            (s(&qdir.join("IMG_1.xmp")), THEIR_EDITS.to_vec()),
            (
                s(&qdir.join("IMG_1.CR2.xmp")),
                b"their second sidecar".to_vec()
            ),
        ]
    );
    let ev = events(&a.db, id);
    assert!(
        ev.iter().any(|(p, k, d)| p == "undo"
            && k == "set-aside"
            && d.contains(&format!("\"aside_entry\":{aside}"))),
        "{ev:?}"
    );

    // Their unit's own undo meets ours at its place: kept, by default.
    assert!(crate::undo(&a.db, aside).is_err());
    assert_eq!(fs::read(&home).unwrap(), OURS);
    // With ours sent back to quarantine, theirs comes home whole.
    crate::undo_with(&a.db, aside, &mut with(Choice::Replace)).unwrap();
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
    assert_eq!(fs::read(&theirs_too).unwrap(), b"their second sidecar");
}

#[test]
fn rename_existing_moves_the_existing_unit_to_the_first_free_name_beside_it() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    // `_1` is someone else's already: it is skipped, never replaced.
    let one = a.dir.join("IMG_1.CR2");
    fs::write(&one, b"unrelated").unwrap();

    let done = crate::undo_with(&a.db, id, &mut with(Choice::RenameExisting)).unwrap();

    assert_eq!(fs::read(&home).unwrap(), OURS);
    assert_eq!(fs::read(&xmp).unwrap(), OUR_EDITS);
    assert_eq!(fs::read(&one).unwrap(), b"unrelated");
    assert_eq!(fs::read(a.dir.join("IMG_2.CR2")).unwrap(), THEIRS);
    assert_eq!(fs::read(a.dir.join("IMG_2.xmp")).unwrap(), THEIR_EDITS);
    assert_eq!((done.set_aside, done.files_back), (2, 2));
    let j2: i64 =
        a.db.conn
            .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
            .unwrap();
    let e2 = a.db.journal_entry(j2).unwrap().unwrap();
    assert_eq!(
        (e2.op.as_str(), e2.status),
        ("set-aside", JournalStatus::Done)
    );
    assert_eq!(
        e2.dst.as_deref(),
        Some(s(&a.dir.join("IMG_2.CR2")).as_str())
    );
}

#[test]
fn rename_returning_skips_a_taken_name_and_leaves_the_existing_file_alone() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    // `_1` taken by a frame alone; `_1`'s sidecar name taken alone would
    // refuse that name just the same — the unit needs every name free.
    fs::write(a.dir.join("IMG_1.CR2"), b"unrelated").unwrap();

    let done = crate::undo_with(&a.db, id, &mut with(Choice::RenameReturning)).unwrap();

    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
    assert_eq!(fs::read(a.dir.join("IMG_1.CR2")).unwrap(), b"unrelated");
    assert!(!a.dir.join("IMG_1.xmp").exists());
    assert_eq!(fs::read(a.dir.join("IMG_2.CR2")).unwrap(), OURS);
    assert_eq!(fs::read(a.dir.join("IMG_2.xmp")).unwrap(), OUR_EDITS);
    assert_eq!(
        (done.files_back, done.set_aside, done.entries_back),
        (2, 0, 1)
    );
    assert_eq!(status(&a.db, id), JournalStatus::Undone);
    // The index follows the photograph to its new name.
    let (path, state): (String, String) =
        a.db.conn
            .query_row(
                "SELECT path, state FROM files WHERE name = 'IMG_2.CR2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!(
        (path, state.as_str()),
        (s(&a.dir.join("IMG_2.CR2")), "present")
    );
}

#[test]
fn a_name_created_between_the_choice_and_the_rename_is_not_replaced() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let one = a.dir.join("IMG_1.CR2");
    let raced = one.clone();
    // Another program creates `IMG_1.CR2` after every look and right
    // before the rename to it.
    let _g = race::before_move(move |_, dst| {
        if dst == raced && !raced.exists() {
            fs::write(&raced, b"created at the last moment")?;
        }
        Ok(())
    });

    crate::undo_with(&a.db, id, &mut with(Choice::RenameReturning)).unwrap();

    assert_eq!(fs::read(&one).unwrap(), b"created at the last moment");
    assert_eq!(fs::read(a.dir.join("IMG_2.CR2")).unwrap(), OURS);
    assert_eq!(fs::read(a.dir.join("IMG_2.xmp")).unwrap(), OUR_EDITS);
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    // Both attempts are in the history, recorded before their renames.
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    let tried: Vec<&str> = entry
        .returned_as
        .iter()
        .map(|t| t[0].to.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(tried, ["IMG_1.CR2", "IMG_2.CR2"]);
}

#[test]
fn lightroom_files_offer_only_keep_and_return_under_a_free_name() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let cat =
        a.db.upsert_catalog(&pc_db::NewCatalog {
            path: s(&a.dir.join("Catalog.lrcat")),
            name: "Catalog".into(),
            disk: String::new(),
            size: 0,
            is_backup: false,
            is_locked: false,
            image_count: Some(1),
            read_error: None,
        })
        .unwrap();
    a.db.replace_catalog_files(cat, &[(s(&home), Some(5), None)])
        .unwrap();

    let e = a.db.journal_entry(id).unwrap().unwrap();
    let c = crate::undo_conflict(&a.db, &e).unwrap().unwrap();
    assert_eq!(c.choices, [Choice::Keep, Choice::RenameReturning]);
    assert!(
        c.limits.iter().any(|l| l.contains("Lightroom")),
        "{:?}",
        c.limits
    );

    for refused in [Choice::Replace, Choice::RenameExisting] {
        let err = crate::undo_with(&a.db, id, &mut with(refused)).unwrap_err();
        assert!(format!("{err:#}").contains(refused.as_str()), "{err:#}");
        assert_eq!(fs::read(&home).unwrap(), THEIRS);
        assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
        assert_eq!(fs::read(q(&home)).unwrap(), OURS);
        assert_eq!(status(&a.db, id), JournalStatus::Done);
    }
    crate::undo_with(&a.db, id, &mut with(Choice::RenameReturning)).unwrap();
    assert_eq!(
        fs::read(&home).unwrap(),
        THEIRS,
        "the catalogued file was touched"
    );
    assert_eq!(fs::read(a.dir.join("IMG_1.CR2")).unwrap(), OURS);
}

#[test]
fn a_foreign_file_that_appears_after_the_preview_is_refused_not_replaced() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let e = a.db.journal_entry(id).unwrap().unwrap();
    let seen = crate::undo_conflict(&a.db, &e).unwrap().unwrap();
    // After the preview, the file at the place is swapped for another.
    fs::rename(&home, a.dir.join("moved-away.CR2")).unwrap();
    fs::write(&home, b"a different file, never reviewed").unwrap();

    let reviewed = Reviewed {
        seen,
        choice: Choice::Replace,
    };
    let err = crate::undo_reviewed(&a.db, id, Some(&reviewed)).unwrap_err();

    assert!(format!("{err:#}").contains("since the preview"), "{err:#}");
    assert_eq!(
        fs::read(&home).unwrap(),
        b"a different file, never reviewed"
    );
    assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
    assert_eq!(status(&a.db, id), JournalStatus::Done);
    let ops: i64 =
        a.db.conn
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap();
    assert_eq!(ops, 1, "nothing was set aside");

    // No conflict when it was reviewed, one now: kept, never replaced.
    let b = archive();
    let (id, home, xmp) = quarantined(&b, "IMG.CR2");
    taken(&home, &xmp);
    let err = crate::undo_reviewed(&b.db, id, None).unwrap_err();
    assert!(crate::conflict_kept(&err).is_some(), "{err:#}");
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
}

#[test]
fn an_occupant_changed_between_the_choice_and_its_move_is_refused_whole() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let theirs = home.clone();
    // The existing photograph is rewritten after the choice, right before
    // it would be set aside.
    let _g = race::before_move(move |src, _| {
        if src == theirs {
            fs::write(&theirs, b"rewritten by its program, a different size")?;
        }
        Ok(())
    });

    let err = crate::undo_with(&a.db, id, &mut with(Choice::RenameExisting)).unwrap_err();

    assert!(format!("{err:#}").contains("nothing"), "{err:#}");
    assert_eq!(
        fs::read(&home).unwrap(),
        b"rewritten by its program, a different size"
    );
    assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
    assert!(!a.dir.join("IMG_1.CR2").exists() && !a.dir.join("IMG_1.xmp").exists());
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
    assert_eq!(status(&a.db, id), JournalStatus::Done);
    let failed: String =
        a.db.conn
            .query_row(
                "SELECT status FROM journal WHERE op = 'set-aside'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(failed, "failed");
}

#[test]
fn a_replace_stopped_after_setting_aside_carries_on_from_there_on_retry() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let ours = q(&home);
    // The return home fails once, after theirs was set aside.
    let mut once = true;
    let _g = race::before_move(move |src, _| {
        if src == ours && std::mem::take(&mut once) {
            return Err(std::io::Error::other("the disk went away for a moment"));
        }
        Ok(())
    });

    let err = crate::undo_with(&a.db, id, &mut with(Choice::Replace)).unwrap_err();

    let stop = crate::stopped_run(&err).expect("what was done before the stop");
    assert_eq!(stop.done.set_aside, 2);
    assert_eq!(status(&a.db, id), JournalStatus::Done);
    assert!(!home.exists(), "the place was made free");
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);

    // Asked again — with the default, since nothing is in the way now.
    let done = crate::undo(&a.db, id).unwrap();
    assert_eq!(done.files_back, 2);
    assert_eq!(fs::read(&home).unwrap(), OURS);
    assert_eq!(fs::read(&xmp).unwrap(), OUR_EDITS);
    let qdir = home.parent().unwrap().join(pc_core::QUARANTINE_DIR);
    assert_eq!(fs::read(qdir.join("IMG_1.CR2")).unwrap(), THEIRS);
    assert_eq!(fs::read(qdir.join("IMG_1.xmp")).unwrap(), THEIR_EDITS);
}

#[test]
fn a_return_under_free_names_interrupted_after_its_frame_finishes_on_retry() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    // What a process killed mid-unit leaves: the attempt recorded, the
    // frame already under its free name, the sidecar still in quarantine.
    let e = a.db.journal_entry(id).unwrap().unwrap();
    let c = crate::undo_conflict(&a.db, &e).unwrap().unwrap();
    let (one, one_xmp) = (a.dir.join("IMG_1.CR2"), a.dir.join("IMG_1.xmp"));
    let note = pc_db::ConflictNote {
        choice: "rename-returning".into(),
        returning: Vec::new(),
        occupants: Vec::new(),
        returned_as: e
            .manifest
            .iter()
            .map(|m| pc_db::ReturnedAs {
                src: m.src.clone(),
                dst: m.dst.clone(),
                to: if m.src == s(&home) {
                    s(&one)
                } else {
                    s(&one_xmp)
                },
            })
            .collect(),
        aside_entry: None,
    };
    a.db.journal_event_conflict(id, "undo", "attempt", "", &note)
        .unwrap();
    fs::rename(q(&home), &one).unwrap();
    assert_eq!(c.returning.len(), 2);

    // The retry finds the frame by its evidence and finishes the unit under
    // the same names — not a new conflict, not a second copy.
    let done = crate::undo(&a.db, id).unwrap();

    assert_eq!(done.files_back, 1);
    assert_eq!(fs::read(&one).unwrap(), OURS);
    assert_eq!(fs::read(&one_xmp).unwrap(), OUR_EDITS);
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
    assert_eq!(status(&a.db, id), JournalStatus::Undone);
}

#[test]
fn a_stranger_at_a_recorded_free_name_is_not_taken_for_the_frame() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let e = a.db.journal_entry(id).unwrap().unwrap();
    let one = a.dir.join("IMG_1.CR2");
    let note = pc_db::ConflictNote {
        choice: "rename-returning".into(),
        returning: Vec::new(),
        occupants: Vec::new(),
        returned_as: vec![pc_db::ReturnedAs {
            src: e.manifest[0].src.clone(),
            dst: e.manifest[0].dst.clone(),
            to: s(&one),
        }],
        aside_entry: None,
    };
    a.db.journal_event_conflict(id, "undo", "attempt", "", &note)
        .unwrap();
    // Not ours: the same name, another object.
    fs::write(&one, OURS).unwrap();

    let err = crate::undo(&a.db, id).unwrap_err();

    assert!(crate::conflict_kept(&err).is_some(), "{err:#}");
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
    assert_eq!(status(&a.db, id), JournalStatus::Done);
}

#[test]
fn a_row_without_evidence_offers_the_choice_and_records_evidence_before_it_moves() {
    let a = archive();
    let home = a.dir.join("old.jpg");
    let held = q(&home);
    fs::create_dir_all(held.parent().unwrap()).unwrap();
    fs::write(&held, OURS).unwrap();
    fs::write(&home, THEIRS).unwrap();
    let id =
        a.db.journal_begin(&pc_db::NewJournalEntry {
            run_id: a.run,
            op: "quarantine-file",
            target_id: None,
            src: &s(&home),
            dst: Some(&s(&held)),
            size: OURS.len() as i64,
            file_count: 1,
            manifest: &[],
        })
        .unwrap();
    a.db.journal_finish(id, JournalStatus::Done, None).unwrap();

    // The file at home is never taken for the one that left: a conflict.
    let e = a.db.journal_entry(id).unwrap().unwrap();
    let c = crate::undo_conflict(&a.db, &e)
        .unwrap()
        .expect("a conflict");
    assert_eq!(c.returning[0].proof, None);
    assert!(crate::undo(&a.db, id).is_err());

    crate::undo_with(&a.db, id, &mut with(Choice::RenameReturning)).unwrap();

    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(a.dir.join("old_1.jpg")).unwrap(), OURS);
    let e = a.db.journal_entry(id).unwrap().unwrap();
    assert!(
        e.manifest[0].proof.is_some(),
        "evidence recorded before the move"
    );
    assert_eq!(e.status, JournalStatus::Undone);
}

#[test]
fn every_conflict_of_a_run_is_asked_and_answered_on_its_own() {
    let a = archive();
    let mut moves = Vec::new();
    for name in ["a.jpg", "b.jpg"] {
        let src = a.dir.join(name);
        fs::write(&src, OURS).unwrap();
        let file_id =
            a.db.upsert_file(
                &pc_db::NewFile {
                    path: s(&src),
                    name: name.into(),
                    size: OURS.len() as i64,
                    mtime: pc_core::time::mtime_unix(&fs::metadata(&src).unwrap()),
                    ..Default::default()
                },
                a.run,
            )
            .unwrap();
        moves.push(pc_organize::Move {
            file_id,
            src: s(&src),
            dst: s(&a.dir.join("2019").join(name)),
            size: OURS.len() as i64,
            mtime: pc_core::time::mtime_unix(&fs::metadata(&src).unwrap()),
            date: pc_organize::Dated {
                ts: 1_562_000_000,
                source: pc_organize::Source::Exif,
                precision: pc_organize::Precision::Day,
            },
            event: "2019".into(),
            renamed_from: None,
        });
    }
    let report = crate::organize(&a.db, a.run, &moves).unwrap();
    assert_eq!(report.done.frames, 2);
    for name in ["a.jpg", "b.jpg"] {
        fs::write(a.dir.join(name), THEIRS).unwrap();
    }

    let mut asked = Vec::new();
    let (back, failed) = crate::undo_run_with(&a.db, a.run, &mut |c| {
        asked.push(c.returning[0].home.clone());
        Ok(Choice::RenameReturning)
    })
    .unwrap();

    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(asked.len(), 2);
    assert_eq!((back.entries_back, back.files_back), (2, 2));
    for name in ["a", "b"] {
        assert_eq!(fs::read(a.dir.join(format!("{name}.jpg"))).unwrap(), THEIRS);
        assert_eq!(fs::read(a.dir.join(format!("{name}_1.jpg"))).unwrap(), OURS);
    }
}
