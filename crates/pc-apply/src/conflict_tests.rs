//! An undo whose original place is taken: each of the user's choices
//! (el-14vx0), on the real consumers — a photograph quarantined by
//! `quarantine_file` with its `.xmp`, and a run of `organize` — on
//! disposable fixtures only.

use crate::{race, Choice, Reviewed, Seen};
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

/// The entry read now, as a preview would, and `choice` made on it — then
/// carried out.
fn chosen(a: &Archive, id: i64, choice: Choice) -> anyhow::Result<crate::Tally> {
    let r = reviewed_now(a, id, choice);
    crate::undo_reviewed(&a.db, id, Some(&r))
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
fn rename_returning_skips_a_taken_name_and_leaves_the_existing_file_alone() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    // `_1` taken by a frame alone; `_1`'s sidecar name taken alone would
    // refuse that name just the same — the unit needs every name free.
    fs::write(a.dir.join("IMG_1.CR2"), b"unrelated").unwrap();

    let done = chosen(&a, id, Choice::RenameReturning).unwrap();

    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
    assert_eq!(fs::read(a.dir.join("IMG_1.CR2")).unwrap(), b"unrelated");
    assert!(!a.dir.join("IMG_1.xmp").exists());
    assert_eq!(fs::read(a.dir.join("IMG_2.CR2")).unwrap(), OURS);
    assert_eq!(fs::read(a.dir.join("IMG_2.xmp")).unwrap(), OUR_EDITS);
    assert_eq!((done.files_back, done.entries_back), (2, 1));
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

    chosen(&a, id, Choice::RenameReturning).unwrap();

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
    let c = crate::undo_conflict(&e).unwrap().unwrap();
    assert_eq!(c.choices, [Choice::Keep, Choice::RenameReturning]);
    // Nothing that could touch the catalogued file exists to be chosen.
    for word in ["replace", "rename-existing"] {
        assert_eq!(Choice::parse(word), None);
    }
    chosen(&a, id, Choice::RenameReturning).unwrap();
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
    let seen = crate::undo_conflict(&e).unwrap().unwrap();
    // After the preview, the file at the place is swapped for another.
    fs::rename(&home, a.dir.join("moved-away.CR2")).unwrap();
    fs::write(&home, b"a different file, never reviewed").unwrap();

    let reviewed = Reviewed {
        seen: Seen::Taken(seen),
        choice: Choice::RenameReturning,
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
    assert_eq!(ops, 1, "no other entry was written");
    assert!(!a.dir.join("IMG_1.CR2").exists());

    // No conflict when it was reviewed, one now: refused as changed since
    // the preview — nothing decided for anyone, never replaced.
    let b = archive();
    let (id, home, xmp) = quarantined(&b, "IMG.CR2");
    taken(&home, &xmp);
    let err = crate::undo_reviewed(&b.db, id, None).unwrap_err();
    let decided = crate::outcome_of(&err).decisions;
    assert_eq!(decided.len(), 1, "{err:#}");
    assert_eq!(decided[0].outcome, crate::Outcome::ChangedSincePreview);
    assert_eq!(decided[0].choice, None);
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
}

#[test]
fn a_return_under_free_names_interrupted_after_its_frame_finishes_on_retry() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    // What a process killed mid-unit leaves: the attempt recorded, the
    // frame already under its free name, the sidecar still in quarantine.
    let e = a.db.journal_entry(id).unwrap().unwrap();
    let c = crate::undo_conflict(&e).unwrap().unwrap();
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
        outcome: None,
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
        outcome: None,
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

    let seen = crate::run_seen(&a.db, a.run).unwrap();
    let asked: Vec<String> = seen
        .iter()
        .filter_map(|s| s.conflict())
        .map(|c| c.returning[0].home.clone())
        .collect();
    let reviewed = seen
        .into_iter()
        .map(|s| {
            (
                s.journal_id(),
                Reviewed {
                    seen: s,
                    choice: Choice::RenameReturning,
                },
            )
        })
        .collect();
    let (back, failed) = crate::undo_run_reviewed(&a.db, a.run, &reviewed).unwrap();

    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(asked.len(), 2);
    assert_eq!((back.entries_back, back.files_back), (2, 2));
    for name in ["a", "b"] {
        assert_eq!(fs::read(a.dir.join(format!("{name}.jpg"))).unwrap(), THEIRS);
        assert_eq!(fs::read(a.dir.join(format!("{name}_1.jpg"))).unwrap(), OURS);
    }
}

// Independent reviewer probes: disposable fixtures, no production edits.
#[cfg(unix)]
fn review_metadata(p: &Path) -> (u64, u64, u32, u32, u32, u64, i64, i64, Vec<u8>) {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(p).unwrap();
    (
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.nlink(),
        m.mtime(),
        m.mtime_nsec(),
        fs::read(p).unwrap(),
    )
}

#[test]
fn reviewer_a_new_existing_companion_during_choice_must_refuse_the_whole_unit() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let before = review_metadata(&home);
    let r = reviewed_now(&a, id, Choice::RenameReturning);
    fs::write(a.dir.join("IMG.aae"), b"late foreign edits").unwrap();
    let result = crate::undo_reviewed(&a.db, id, Some(&r));
    eprintln!(
        "NEW_COMPANION_RESULT={result:?}; late_sidecar={:?}; home={:?}; aside={:?}",
        fs::read(a.dir.join("IMG.aae")),
        fs::read(&home),
        fs::read(a.dir.join("IMG_1.CR2"))
    );
    assert!(
        result.is_err(),
        "changed unit was accepted and its new companion left behind"
    );
    assert_eq!(review_metadata(&home), before);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
}

#[test]
fn reviewer_existing_disappears_during_choice_refuses_and_keeps_payloads() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let before = review_metadata(&home);
    let side_before = review_metadata(&xmp);
    let away = a.dir.join("external-move.CR2");
    let r = reviewed_now(&a, id, Choice::RenameReturning);
    fs::rename(&home, &away).unwrap();
    let err = crate::undo_reviewed(&a.db, id, Some(&r)).unwrap_err();
    assert!(format!("{err:#}").contains("nothing"), "{err:#}");
    assert_eq!(review_metadata(&away), before);
    assert_eq!(review_metadata(&xmp), side_before);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
}

#[test]
fn reviewer_a_very_long_suffix_refuses_without_touching_foreign_metadata() {
    let a = archive();
    let name = format!("{}.CR2", "n".repeat(250));
    let (id, home, xmp) = quarantined(&a, &name);
    taken(&home, &xmp);
    let before = review_metadata(&home);
    let side = review_metadata(&xmp);
    let e = chosen(&a, id, Choice::RenameReturning).unwrap_err();
    eprintln!("LONG_NAME_ERROR={e:#}");
    assert_eq!(review_metadata(&home), before);
    assert_eq!(review_metadata(&xmp), side);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
}

#[test]
fn reviewer_unicode_case_companion_collision_is_skipped_as_a_unit() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "Été.CR2");
    taken(&home, &xmp);
    let collision = a.dir.join("Été_1.XMP");
    fs::write(&collision, b"foreign uppercase sidecar").unwrap();
    let before = review_metadata(&collision);
    let done = chosen(&a, id, Choice::RenameReturning).unwrap();
    assert_eq!(done.files_back, 2);
    assert_eq!(review_metadata(&collision), before);
    // On case-insensitive native APFS the uppercase name collides with .xmp.
    if a.dir.join("Été_1.xmp").exists()
        && fs::read(a.dir.join("Été_1.xmp")).unwrap() == b"foreign uppercase sidecar"
    {
        assert_eq!(fs::read(a.dir.join("Été_2.CR2")).unwrap(), OURS);
    }
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
}

/// el-14vx0 B3. An operation interrupted after its files reached quarantine,
/// whose places another program has taken since: reconciling it offers the
/// same choices as an undo, through the same function. The default keeps it
/// — written down under `reconcile`, the entry still pending, nothing moved
/// — and a choice brings it back as that choice says.
#[test]
fn an_interrupted_entry_whose_place_is_taken_offers_the_same_choices_on_reconcile() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    a.db.journal_finish(id, JournalStatus::Pending, None)
        .unwrap();
    taken(&home, &xmp);

    let entry = a.db.journal_entry(id).unwrap().unwrap();
    let c = crate::reconcile_conflict(&entry)
        .unwrap()
        .expect("a conflict");
    assert_eq!(c.choices, Choice::ALL.to_vec());
    assert!(crate::undo_conflict(&entry).unwrap().is_none());

    let e = crate::reconcile_undo(&a.db, id).unwrap_err();
    assert!(crate::conflict_kept(&e).is_some(), "{e:#}");
    assert_eq!(status(&a.db, id), JournalStatus::Pending);
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
    assert!(events(&a.db, id)
        .iter()
        .any(|(p, k, d)| p == "reconcile" && k == "kept" && d.contains("\"choice\":\"keep\"")));

    let r = reviewed_now(&a, id, Choice::RenameReturning);
    let r = crate::reconcile_reviewed(&a.db, id, Some(&r)).unwrap();
    assert_eq!(r.done.files_back, 2);
    assert_eq!(status(&a.db, id), JournalStatus::Undone);
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(&xmp).unwrap(), THEIR_EDITS);
    assert_eq!(fs::read(a.dir.join("IMG_1.CR2")).unwrap(), OURS);
    assert_eq!(fs::read(a.dir.join("IMG_1.xmp")).unwrap(), OUR_EDITS);
    assert!(events(&a.db, id)
        .iter()
        .any(|(p, k, _)| p == "reconcile" && k == "done"));
}

/// el-14vx0 B4. "Keep", once chosen and confirmed, is carried out as such:
/// written down and typed in the result, and nothing moves. (Once the
/// place has become free, the conflict is no longer the one reviewed: that
/// is refused as changed since the preview, below.)
#[test]
fn a_reviewed_keep_is_carried_out_written_down_and_typed() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    let seen = crate::undo_conflict(&entry).unwrap().unwrap();

    let e = crate::undo_reviewed(
        &a.db,
        id,
        Some(&Reviewed {
            seen: Seen::Taken(seen),
            choice: Choice::Keep,
        }),
    )
    .unwrap_err();

    assert!(crate::conflict_kept(&e).is_some(), "{e:#}");
    let decided = crate::outcome_of(&e).decisions;
    assert_eq!(
        decided,
        [crate::Decision {
            journal_id: id,
            choice: Some(Choice::Keep),
            outcome: crate::Outcome::Kept,
        }]
    );
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
    assert_eq!(fs::read(q(&xmp)).unwrap(), OUR_EDITS);
    assert_eq!(status(&a.db, id), JournalStatus::Done);
    assert!(events(&a.db, id).iter().any(|(p, k, d)| p == "undo"
        && k == "kept"
        && d.contains("\"choice\":\"keep\"")
        && d.contains("\"outcome\":\"kept\"")));
}

// ---- el-14vx0 round 3: the preview is binding (rejection el-zvg9s) --------

/// What a preview reads for the entry now — of an undo, or of the
/// reconciliation of an interrupted one — with `choice` made on it.
fn reviewed_now(a: &Archive, id: i64, choice: Choice) -> Reviewed {
    let entry = a.db.journal_entry(id).unwrap().unwrap();
    let seen = if entry.status == JournalStatus::Pending {
        crate::reconcile_seen(&entry)
    } else {
        crate::undo_seen(&entry)
    };
    Reviewed {
        seen: seen.unwrap(),
        choice,
    }
}

fn changed_event(a: &Archive, id: i64, choice: Choice) -> bool {
    let needle = format!("\"choice\":\"{}\"", choice.as_str());
    events(&a.db, id).iter().any(|(_, k, d)| {
        k == "refused" && d.contains(&needle) && d.contains("\"outcome\":\"changed-since-preview\"")
    })
}

/// R2-B2: the existing unit reviewed in the preview is gone by the time the
/// choice is carried out. The place is free, but the choice was made on a
/// conflict that is no longer there: nothing comes back by an ordinary
/// undo or under any other name; the refusal is written down and says to
/// preview again.
#[test]
fn a_reviewed_conflict_that_vanished_is_refused_never_undone_otherwise() {
    for choice in Choice::ALL {
        let a = archive();
        let (id, home, xmp) = quarantined(&a, "IMG.CR2");
        taken(&home, &xmp);
        let r = reviewed_now(&a, id, choice);
        fs::rename(&home, a.dir.join("theirs.CR2")).unwrap();
        fs::rename(&xmp, a.dir.join("theirs.xmp")).unwrap();

        let e = crate::undo_reviewed(&a.db, id, Some(&r)).unwrap_err();

        assert!(format!("{e:#}").contains("preview"), "{choice:?}: {e:#}");
        assert!(!home.exists() && !xmp.exists(), "{choice:?}");
        assert!(!a.dir.join("IMG_1.CR2").exists(), "{choice:?}");
        assert_eq!(fs::read(q(&home)).unwrap(), OURS);
        assert_eq!(fs::read(q(&xmp)).unwrap(), OUR_EDITS);
        assert_eq!(status(&a.db, id), JournalStatus::Done);
        assert!(
            changed_event(&a, id, choice),
            "{choice:?}: {:?}",
            events(&a.db, id)
        );
    }
}

/// R2-B3: the existing frame changed after the preview. The reviewed choice
/// is refused, nothing moves, and the confirmed choice with its refusal is
/// in the structured history.
#[test]
fn a_reviewed_conflict_that_changed_is_refused_and_written_down_with_its_choice() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);
    let r = reviewed_now(&a, id, Choice::RenameReturning);
    fs::write(&home, b"an ordinary edit after the preview").unwrap();

    let e = crate::undo_reviewed(&a.db, id, Some(&r)).unwrap_err();

    assert!(format!("{e:#}").contains("preview"), "{e:#}");
    assert_eq!(
        fs::read(&home).unwrap(),
        b"an ordinary edit after the preview"
    );
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
    assert!(
        changed_event(&a, id, Choice::RenameReturning),
        "{:?}",
        events(&a.db, id)
    );
}

/// Point 1, "appeared": an entry previewed with its place free, and taken by
/// the time the job reaches it. It was never reviewed as a conflict: it is
/// refused as changed since the preview, not kept or decided silently.
#[test]
fn a_conflict_that_appeared_after_the_preview_is_refused_as_changed() {
    let a = archive();
    let (id, home, xmp) = quarantined(&a, "IMG.CR2");
    taken(&home, &xmp);

    let e = crate::undo_reviewed(&a.db, id, None).unwrap_err();

    assert!(format!("{e:#}").contains("preview"), "{e:#}");
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(q(&home)).unwrap(), OURS);
    assert!(events(&a.db, id)
        .iter()
        .any(|(_, k, d)| k == "refused" && d.contains("\"outcome\":\"changed-since-preview\"")));
}

/// Point 2: a row written without evidence is offered only "keep"; a choice
/// that moves anything is refused whole and written down.
#[test]
fn a_row_without_evidence_is_offered_only_keep() {
    let a = archive();
    let home = a.dir.join("old.jpg");
    let held = q(&home);
    fs::create_dir_all(held.parent().unwrap()).unwrap();
    fs::write(&held, OURS).unwrap();
    fs::write(held.with_extension("xmp"), OUR_EDITS).unwrap();
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

    let r = reviewed_now(&a, id, Choice::RenameReturning);
    assert_eq!(r.seen.conflict().unwrap().choices, vec![Choice::Keep]);
    for choice in Choice::ALL {
        let r = reviewed_now(&a, id, choice);
        assert!(crate::undo_reviewed(&a.db, id, Some(&r)).is_err());
        assert!(crate::undo(&a.db, id).is_err());
    }
    assert_eq!(fs::read(&home).unwrap(), THEIRS);
    assert_eq!(fs::read(&held).unwrap(), OURS);
    assert_eq!(fs::read(held.with_extension("xmp")).unwrap(), OUR_EDITS);
    assert!(!a.dir.join("old_1.jpg").exists());
    assert_eq!(status(&a.db, id), JournalStatus::Done);
    assert!(a.db.journal_entry(id).unwrap().unwrap().manifest.is_empty());
}

/// Point 3: a carried out choice is written down with its outcome.
#[test]
fn a_carried_out_choice_is_written_down_with_its_outcome() {
    let (choice, outcome) = (Choice::RenameReturning, "renamed-returning");
    {
        let a = archive();
        let (id, home, xmp) = quarantined(&a, "IMG.CR2");
        taken(&home, &xmp);
        let r = reviewed_now(&a, id, choice);
        let done = crate::undo_reviewed(&a.db, id, Some(&r)).unwrap();
        assert_eq!(done.decisions.len(), 1, "{choice:?}");
        assert_eq!(done.decisions[0].choice, Some(choice));
        assert_eq!(done.decisions[0].outcome.as_str(), outcome);
        assert!(done.summary().contains(outcome), "{}", done.summary());
        let needle = format!("\"outcome\":\"{outcome}\"");
        assert!(
            events(&a.db, id)
                .iter()
                .any(|(_, _, d)| d.contains(&needle)),
            "{choice:?}: {:?}",
            events(&a.db, id)
        );
    }
}

// ---- el-14vx0 round 4: reproducers of rejection el-66rxn (R3-B1..B3) -----
//
// Adapted from the reviewer's probes (/tmp/el-14vx0-review-r3-el-67ku):
// after the user's decision (a), only "keep" and "return as *_1" remain.

mod review_round3 {
    use super::*;

    /// R3-B1/B3: every member of either side — the frame, its `.xmp`, and an
    /// `.aae` that was not there at the preview — edited, renamed away or
    /// vanished after the preview refuses the unit: nothing moves, the
    /// refusal is typed and journaled as changed since the preview.
    #[test]
    fn r3_each_side_each_member_change_is_binding() {
        let mut failures = Vec::new();
        for choice in [Choice::Keep, Choice::RenameReturning] {
            for side in ["returning", "occupant"] {
                for member in ["frame", "xmp", "extra"] {
                    for change in ["edit", "rename", "vanish"] {
                        if member == "extra" && change != "edit" {
                            continue;
                        }
                        let a = archive();
                        let (id, home, xmp) = quarantined(&a, "IMG.CR2");
                        taken(&home, &xmp);
                        let r = reviewed_now(&a, id, choice);
                        let path = match (side, member) {
                            ("returning", "extra") => q(&home).with_extension("aae"),
                            (_, "extra") => home.with_extension("aae"),
                            ("returning", "frame") => q(&home),
                            ("returning", _) => q(&xmp),
                            (_, "frame") => home.clone(),
                            _ => xmp.clone(),
                        };
                        let away = path.with_file_name("externally-moved.bin");
                        match change {
                            "edit" => fs::write(&path, b"ordinary external change").unwrap(),
                            _ => fs::rename(&path, &away).unwrap(),
                        }
                        let paths = [
                            home.clone(),
                            xmp.clone(),
                            q(&home),
                            q(&xmp),
                            path.clone(),
                            away.clone(),
                        ];
                        let before: Vec<_> = paths
                            .iter()
                            .filter(|p| p.exists())
                            .map(|p| (p.clone(), review_metadata(p)))
                            .collect();
                        let result = crate::undo_reviewed(&a.db, id, Some(&r));
                        let preserved = before
                            .iter()
                            .all(|(p, m)| p.exists() && review_metadata(p) == *m);
                        let typed = result
                            .as_ref()
                            .err()
                            .map(crate::outcome_of)
                            .is_some_and(|t| {
                                t.decisions
                                    .iter()
                                    .any(|d| d.outcome == crate::Outcome::ChangedSincePreview)
                            });
                        let event = changed_event(&a, id, choice);
                        if !(preserved && typed && event) {
                            failures.push(format!(
                                "{choice:?}/{side}/{member}/{change}: result={result:?} \
                                 preserved={preserved} typed={typed} event={event}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// R3-B3: a "keep" answered after the existing file changed is not a
    /// keep of what was shown — it is changed since the preview. (The
    /// callback that asked while reading, `undo_with`, is gone: every
    /// answer is now given on a preview and bound to it.)
    #[test]
    fn r3_interactive_keep_after_change_is_changed_not_kept() {
        let a = archive();
        let (id, home, xmp) = quarantined(&a, "IMG.CR2");
        taken(&home, &xmp);
        let r = reviewed_now(&a, id, Choice::Keep);
        fs::write(&home, b"edit while answering keep").unwrap();
        let e = crate::undo_reviewed(&a.db, id, Some(&r)).unwrap_err();
        assert_eq!(fs::read(&home).unwrap(), b"edit while answering keep");
        assert_eq!(fs::read(q(&home)).unwrap(), OURS);
        assert!(
            changed_event(&a, id, Choice::Keep),
            "{e:#} {:?}",
            events(&a.db, id)
        );
    }

    /// R3-B2: a row written without evidence is keep-only even when its
    /// place is free — on undo and on reconciliation alike.
    #[test]
    fn r3_legacy_free_destination_is_keep_only() {
        let mut failures = Vec::new();
        for pending in [false, true] {
            let a = archive();
            let home = a.dir.join("old.jpg");
            let held = q(&home);
            fs::create_dir_all(held.parent().unwrap()).unwrap();
            fs::write(&held, OURS).unwrap();
            fs::write(held.with_extension("xmp"), OUR_EDITS).unwrap();
            let id =
                a.db.journal_begin(&pc_db::NewJournalEntry {
                    run_id: a.run,
                    op: "quarantine-file",
                    target_id: None,
                    src: &s(&home),
                    dst: Some(&s(&held)),
                    size: OURS.len() as i64,
                    file_count: 2,
                    manifest: &[],
                })
                .unwrap();
            if !pending {
                a.db.journal_finish(id, JournalStatus::Done, None).unwrap();
            }
            let before = review_metadata(&held);
            let edits = review_metadata(&held.with_extension("xmp"));
            let r = reviewed_now(&a, id, Choice::RenameReturning);
            assert!(matches!(&r.seen, Seen::Held(h) if h.legacy), "{:?}", r.seen);
            let result = if pending {
                crate::reconcile_reviewed(&a.db, id, Some(&r)).map(|r| r.done)
            } else {
                crate::undo_reviewed(&a.db, id, Some(&r))
            };
            let kept = result
                .as_ref()
                .err()
                .map(crate::outcome_of)
                .is_some_and(|t| {
                    t.decisions.iter().any(|d| {
                        d.outcome == crate::Outcome::Kept && d.choice == Some(Choice::Keep)
                    })
                });
            // Nor by the plain undo or reconciliation.
            let plain = if pending {
                crate::reconcile_undo(&a.db, id).map(|r| r.done)
            } else {
                crate::undo(&a.db, id)
            };
            let preserved = held.exists()
                && held.with_extension("xmp").exists()
                && review_metadata(&held) == before
                && review_metadata(&held.with_extension("xmp")) == edits
                && !home.exists()
                && plain.is_err()
                && a.db.journal_entry(id).unwrap().unwrap().manifest.is_empty();
            if !(preserved && kept) {
                failures.push(format!(
                    "pending={pending} result={result:?} plain={plain:?} status={:?} events={:?}",
                    status(&a.db, id),
                    events(&a.db, id)
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
