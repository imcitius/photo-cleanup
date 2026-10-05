//! Round four of el-usdqi: the blockers B1–B6 of review el-1y8uo, each as
//! the reviewer's reproducer (adapted only to this crate's test helpers).
//!
//! B1 — nothing is ever unlinked by apply, undo, reconcile, organize or the
//! litter sweep (user decision 2026-10-04: POSIX has no conditional unlink,
//! so every check-then-unlink can remove a stranger's entry substituted
//! after the check). Folders a run empties stay where they are.
//! B2 — recorded evidence this build cannot read fails closed; it is never
//! taken for a row that recorded no evidence.
//! B3 — a database failure after a physical move still returns the typed
//! partial with what actually moved.
//! B4 — a legacy reconciliation writes the evidence it adopts before it
//! moves anything, so its retry continues by proof.
//! B6 — every forward outcome appends a typed event, note or no note.

use super::*;

// ------------------------------------------------------------------ B1 ----

/// D3 (el-1y8uo B1). A reorganisation empties a folder. That folder is not
/// removed — not the one it emptied, and not a stranger's empty folder that
/// took its name between any check and any removal. The folder name
/// contains `prune-empty-review`, the marker of the reviewer's native
/// `unlinkat` interposer (`/tmp/el-usdqi-review-87e1804/fault.c`): run under
/// it, the interposer swaps a foreign 0700 folder in at the removal call,
/// and the stranger must still be there afterwards; run without it, the
/// emptied folder itself must still be there.
#[cfg(unix)]
#[test]
fn organize_leaves_the_folder_it_emptied_and_any_stranger_at_its_name() {
    use std::os::unix::fs::MetadataExt;
    let a = archive();
    let old = a.dir.join("prune-empty-review");
    fs::create_dir_all(&old).unwrap();
    let before = fs::symlink_metadata(&old).unwrap();
    let src = old.join("frame.jpg");
    fs::write(&src, b"frame").unwrap();
    let dest = a.dir.join("new");
    fs::create_dir_all(&dest).unwrap();
    let m = organized(&a, &src, &dest.join("frame.jpg"));

    let report = crate::organize(&a.db, a.run, &[m]).unwrap();

    assert_eq!(report.done.frames, 1, "{report:?}");
    let now = fs::symlink_metadata(&old).expect("the emptied folder's name was removed");
    assert!(now.is_dir());
    let own = PathBuf::from(format!("{}.retained-own", old.display()));
    if own.exists() {
        // The native interposer swapped a stranger in at the removal call.
        assert_eq!(now.mode() & 0o777, 0o700, "not the stranger's folder");
    } else {
        assert_eq!(
            (now.dev(), now.ino()),
            (before.dev(), before.ino()),
            "the emptied folder was replaced"
        );
    }
}

/// The same for an undo: the folders in quarantine it empties on the way
/// out stay where they are.
#[test]
fn undo_leaves_the_quarantine_folders_it_emptied() {
    let a = archive();
    let sub = a.dir.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let src = sub.join("frame.arw");
    fs::write(&src, b"frame one").unwrap();
    let c = candidate(&a.db, a.run, &src);
    crate::apply(&a.db, a.run, &[c], None).unwrap();
    let id = last_journal_id(&a.db);
    let held = PathBuf::from(a.db.journal_entry(id).unwrap().unwrap().dst.unwrap());
    let folder = held.parent().unwrap().to_path_buf();
    assert!(folder.is_dir());

    crate::undo(&a.db, id).unwrap();

    assert_eq!(fs::read(&src).unwrap(), b"frame one");
    assert!(
        folder.is_dir(),
        "the emptied quarantine folder {} was removed",
        folder.display()
    );
}

// ------------------------------------------------------------------ B2 ----

/// B2 (el-1y8uo). A row records evidence in a form this build does not
/// understand — valid JSON from a later version. It is not a row without
/// evidence: the undo and the reconciliation both refuse, nothing moves, the
/// raw record stays as it was, and the refusal names both paths.
#[cfg(unix)]
#[test]
fn unsupported_proof_schema_must_not_become_legacy_adoption() {
    use std::os::unix::fs::MetadataExt;
    for status in [JournalStatus::Done, JournalStatus::Pending] {
        let a = archive();
        fs::create_dir_all(&a.quarantine).unwrap();
        let home = a.dir.join("frame.arw");
        let held = a.quarantine.join("frame.arw");
        fs::write(&held, b"owned original").unwrap();
        let id =
            a.db.journal_begin(&pc_db::NewJournalEntry {
                run_id: a.run,
                op: "quarantine-file",
                target_id: None,
                src: &home.display().to_string(),
                dst: Some(&held.display().to_string()),
                size: 14,
                file_count: 1,
                manifest: &[],
            })
            .unwrap();
        a.db.conn
            .execute(
                "UPDATE journal SET status=?1 WHERE id=?2",
                rusqlite::params![status.as_str(), id],
            )
            .unwrap();
        let md = fs::symlink_metadata(&held).unwrap();
        let proof = pc_core::proof::Proof::of(&md).unwrap();
        // A future proof version/schema is valid JSON, but not understood by
        // this binary.
        let json = format!(
            r#"[{{"src":{:?},"dst":{:?},"proof":{{"v":2,"dev":{},"ino":{},"kind":"future-file","size":14}}}}]"#,
            home.display().to_string(),
            held.display().to_string(),
            proof.dev,
            proof.ino
        );
        a.db.conn
            .execute(
                "UPDATE journal SET manifest=?1 WHERE id=?2",
                rusqlite::params![json, id],
            )
            .unwrap();
        fs::rename(&held, a.dir.join("saved-original.arw")).unwrap();
        fs::write(&held, b"foreign held file").unwrap();
        let before = fs::symlink_metadata(&held).unwrap();

        let r = match status {
            JournalStatus::Done => crate::undo(&a.db, id).map(|_| ()),
            _ => crate::reconcile_undo(&a.db, id).map(|_| ()),
        };

        let at = if home.exists() { &home } else { &held };
        assert_eq!(fs::read(at).unwrap(), b"foreign held file");
        assert_eq!(fs::symlink_metadata(at).unwrap().ino(), before.ino());
        let e = r.expect_err(
            "unsupported recorded identity must refuse, not discard evidence and adopt a foreign held file",
        );
        assert!(held.exists() && !home.exists(), "the foreign file moved");
        let shown = format!("{e:#}");
        assert!(shown.contains(&home.display().to_string()), "{shown}");
        assert!(shown.contains(&held.display().to_string()), "{shown}");
        let raw: String =
            a.db.conn
                .query_row("SELECT manifest FROM journal WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap();
        assert_eq!(raw, json, "the recorded evidence was rewritten");
        assert_eq!(journal_row(&a.db, id).0, status.as_str());
        // The read-only preview says the same, and the purge refuses too.
        if status == JournalStatus::Done {
            let entry = a.db.journal_entry(id).unwrap().unwrap();
            assert!(crate::undo_preview(&entry).is_err());
            assert!(
                crate::purge_entry_controlled(&a.db, id, &pc_core::work::Control::default())
                    .is_err()
            );
            assert_eq!(fs::read(&held).unwrap(), b"foreign held file");
        }
    }
}

// ------------------------------------------------------------------ B3 ----

#[test]
fn organize_second_journal_failure_keeps_first_move_in_typed_result() {
    let a = archive();
    let one = a.dir.join("one.arw");
    let two = a.dir.join("two.arw");
    fs::write(&one, b"frame one").unwrap();
    fs::write(&two, b"frame two").unwrap();
    let ms = [
        organized(&a, &one, &a.dir.join("new/one.arw")),
        organized(&a, &two, &a.dir.join("new/two.arw")),
    ];
    a.db.conn
        .execute_batch(
            "CREATE TRIGGER fail_second BEFORE INSERT ON journal WHEN NEW.src LIKE '%/two.arw' \
             BEGIN SELECT RAISE(FAIL,'synthetic second journal failure'); END;",
        )
        .unwrap();
    let e = crate::organize(&a.db, a.run, &ms).unwrap_err();
    assert_eq!(fs::read(a.dir.join("new/one.arw")).unwrap(), b"frame one");
    assert_eq!(fs::read(&two).unwrap(), b"frame two");
    assert_eq!(
        crate::stopped_run(&e).map(|s| (s.done.frames, s.done.bytes)),
        Some((1, 9)),
        "a completed frame vanished from the caller-visible result: {e:#}"
    );
}

#[test]
fn litter_journal_failure_counts_litter_already_moved() {
    let a = archive();
    let dir = a.dir.join("old");
    fs::create_dir(&dir).unwrap();
    let src = dir.join("frame.arw");
    fs::write(&src, b"frame one").unwrap();
    fs::write(dir.join(".DS_Store"), b"finder data").unwrap();
    let m = organized(&a, &src, &a.dir.join("new/frame.arw"));
    a.db.conn
        .execute_batch(
            "CREATE TRIGGER fail_litter BEFORE UPDATE OF status ON journal \
             WHEN NEW.src LIKE '%/.DS_Store' AND NEW.status='done' \
             BEGIN SELECT RAISE(FAIL,'synthetic litter journal failure'); END;",
        )
        .unwrap();
    let e = crate::organize(&a.db, a.run, &[m]).unwrap_err();
    assert_eq!(
        fs::read(a.quarantine.join("old/.DS_Store")).unwrap(),
        b"finder data"
    );
    let stop = crate::stopped_run(&e).expect("a typed stop");
    assert_eq!(
        (stop.done.frames, stop.done.litter, stop.done.bytes),
        (1, 1, 20),
        "moved litter and bytes must survive journal refusal: {e:#}"
    );
    // The litter's row could not be closed: the stop names it as pending,
    // and does not offer the ordinary undo as if everything were recorded.
    let litter_row: i64 =
        a.db.conn
            .query_row(
                "SELECT id FROM journal WHERE src LIKE '%/.DS_Store'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(journal_row(&a.db, litter_row).0, "pending");
    assert_eq!(stop.pending, vec![litter_row], "{e:#}");
    assert!(
        format!("{e:#}").contains(&format!("#{litter_row}")),
        "the pending entry is not named: {e:#}"
    );
}

#[test]
fn undo_persistence_failure_keeps_returned_file_in_typed_result() {
    let a = archive();
    fs::create_dir_all(&a.quarantine).unwrap();
    let held = a.quarantine.join("frame.arw");
    let home = a.dir.join("frame.arw");
    fs::write(&held, b"frame one").unwrap();
    let pair = pc_db::Moved {
        src: home.display().to_string(),
        dst: held.display().to_string(),
        proof: pc_core::proof::Proof::of(&fs::symlink_metadata(&held).unwrap()),
    };
    let id =
        a.db.journal_begin(&pc_db::NewJournalEntry {
            run_id: a.run,
            op: "quarantine-file",
            target_id: None,
            src: &pair.src,
            dst: Some(&pair.dst),
            size: 9,
            file_count: 1,
            manifest: std::slice::from_ref(&pair),
        })
        .unwrap();
    a.db.conn
        .execute("UPDATE journal SET status='done' WHERE id=?1", [id])
        .unwrap();
    a.db.conn
        .execute_batch(
            "CREATE TRIGGER fail_undo BEFORE UPDATE OF status ON journal WHEN NEW.status='undone' \
             BEGIN SELECT RAISE(FAIL,'synthetic undo persistence failure'); END;",
        )
        .unwrap();
    let e = crate::undo(&a.db, id).unwrap_err();
    assert_eq!(fs::read(&home).unwrap(), b"frame one");
    assert_eq!(
        crate::stopped_run(&e).map(|s| s.done.files_back),
        Some(1),
        "returned file must survive persistence refusal: {e:#}"
    );
    // Nothing claims the undo was recorded: no success event without the
    // state that goes with it, and asking again finishes it.
    assert!(
        !a.db
            .journal_events(id)
            .unwrap()
            .iter()
            .any(|ev| ev.phase == "undo" && ev.kind == "done"),
        "a success event without the state transition"
    );
    a.db.conn.execute_batch("DROP TRIGGER fail_undo").unwrap();
    crate::undo(&a.db, id).unwrap();
    assert_eq!(journal_row(&a.db, id).0, "undone");
}

// ------------------------------------------------------------------ B4 ----

#[test]
fn legacy_reconcile_partial_can_retry_without_guessing_home() {
    let a = archive();
    fs::create_dir_all(&a.quarantine).unwrap();
    let home = a.dir.join("frame.arw");
    let held = a.quarantine.join("frame.arw");
    let side = home.with_extension("xmp");
    let held_side = held.with_extension("xmp");
    fs::write(&held, b"frame one").unwrap();
    fs::write(&held_side, b"our edits").unwrap();
    let list = [
        pc_db::Moved::new(home.display().to_string(), held.display().to_string()),
        pc_db::Moved::new(side.display().to_string(), held_side.display().to_string()),
    ];
    let id =
        a.db.journal_begin(&pc_db::NewJournalEntry {
            run_id: a.run,
            op: "quarantine-file",
            target_id: None,
            src: &list[0].src,
            dst: Some(&list[0].dst),
            size: 18,
            file_count: 2,
            manifest: &list,
        })
        .unwrap();
    let at = side.clone();
    let g = race::before_move(move |_, d| {
        if d == at {
            fs::write(d, b"foreign edits")?;
        }
        Ok(())
    });
    assert!(crate::reconcile_undo(&a.db, id).is_err());
    drop(g);
    assert_eq!(fs::read(&side).unwrap(), b"foreign edits");
    assert_eq!(fs::read(&held_side).unwrap(), b"our edits");
    fs::rename(&side, a.dir.join("saved-foreign.xmp")).unwrap();

    let r = crate::reconcile_undo(&a.db, id);
    assert!(
        r.is_ok(),
        "the operation must persist adopted held proof before its first move: {r:?}"
    );
    assert_eq!(fs::read(&home).unwrap(), b"frame one");
    assert_eq!(fs::read(&side).unwrap(), b"our edits");
    assert_eq!(
        fs::read(a.dir.join("saved-foreign.xmp")).unwrap(),
        b"foreign edits"
    );
}

// ------------------------------------------------------------------ B6 ----

/// B6 (el-1y8uo). Every forward outcome — success without any note, a
/// refusal, a partial carry — appends a typed event with structured data,
/// not a generic `note/note` that exists only when a human note does.
#[test]
fn every_forward_outcome_has_a_structured_event() {
    // Success, one frame, no companions, no note.
    let a = archive();
    let src = a.dir.join("frame.arw");
    fs::write(&src, b"frame one").unwrap();
    let c = candidate(&a.db, a.run, &src);
    crate::apply(&a.db, a.run, &[c], None).unwrap();
    let id = last_journal_id(&a.db);
    let ev = a.db.journal_events(id).unwrap();
    let done = ev
        .iter()
        .find(|e| e.phase == "forward" && e.kind == "done")
        .unwrap_or_else(|| panic!("no forward/done event: {ev:?}"));
    let data = done.data.as_deref().expect("structured data");
    assert!(data.contains(&src.display().to_string()), "{data}");
    assert!(!ev.iter().any(|e| e.kind == "note"), "{ev:?}");

    // Refusal: the destination is taken at the move.
    let a = archive();
    let src = a.dir.join("frame.arw");
    fs::write(&src, b"frame one").unwrap();
    let c = candidate(&a.db, a.run, &src);
    let g = race::before_move(|_, d| {
        fs::create_dir_all(d.parent().unwrap())?;
        fs::write(d, b"stranger")
    });
    let _ = crate::apply(&a.db, a.run, &[c], None);
    drop(g);
    let id = last_journal_id(&a.db);
    let ev = a.db.journal_events(id).unwrap();
    assert!(
        ev.iter()
            .any(|e| e.phase == "forward" && e.kind == "refused" && e.data.is_some()),
        "{ev:?}"
    );

    // No partial outcome any more (user decision (c)): the frame moves, its
    // sidecar meets a stranger, and the frame is put back — a refusal,
    // with where the frame was proven to be.
    let a = archive();
    let src = a.dir.join("frame.arw");
    fs::write(&src, b"frame one").unwrap();
    fs::write(src.with_extension("xmp"), b"edits").unwrap();
    let c = candidate(&a.db, a.run, &src);
    let g = race::before_move(|s, d| {
        if s.extension().is_some_and(|e| e == "xmp") {
            fs::write(d, b"stranger")?;
        }
        Ok(())
    });
    let _ = crate::apply(&a.db, a.run, &[c], None);
    drop(g);
    let id = last_journal_id(&a.db);
    let ev = a.db.journal_events(id).unwrap();
    assert!(
        ev.iter()
            .any(|e| e.phase == "forward" && e.kind == "refused" && e.data.is_some()),
        "{ev:?}"
    );
    assert!(!ev.iter().any(|e| e.kind == "partial"), "{ev:?}");
    assert_eq!(fs::read(&src).unwrap(), b"frame one");
}
