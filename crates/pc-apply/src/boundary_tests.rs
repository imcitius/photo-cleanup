//! The boundaries of the independent diagnosis el-5vue3 (el-usdqi).
//!
//! The race and refusal tests prove a move never replaces what is at its
//! destination. These prove what has to hold around it: a file is only
//! taken for ours on evidence recorded when it was ours — never because a
//! name, an old list or an existing entry says so; the journal keeps every
//! attempt instead of the latest one; a stopped or partial run reports what
//! actually moved, companions and service files included, and the sizes
//! the journal keeps are what moved, not what was planned.

use super::*;

/// One journal row of a quarantined photograph, with the evidence a
/// forward move records about it.
fn recorded(src: &Path, dst: &Path) -> pc_db::Moved {
    let md = fs::symlink_metadata(dst).unwrap();
    pc_db::Moved {
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        proof: pc_core::proof::Proof::of(&md),
    }
}

fn row(a: &Archive, home: &Path, held: &Path, manifest: &[pc_db::Moved], s: JournalStatus) -> i64 {
    let id =
        a.db.journal_begin(&pc_db::NewJournalEntry {
            run_id: a.run,
            op: "quarantine-file",
            target_id: None,
            src: &home.display().to_string(),
            dst: Some(&held.display().to_string()),
            size: 9,
            file_count: manifest.len().max(1) as i64,
            manifest,
        })
        .unwrap();
    a.db.journal_finish(id, s, Some("prior recovery evidence"))
        .unwrap();
    id
}

fn note(a: &Archive, id: i64) -> String {
    journal_row(&a.db, id).1
}

fn held_frame(a: &Archive) -> (PathBuf, PathBuf) {
    let home = a.dir.join("frame.arw");
    let held = a.quarantine.join("frame.arw");
    fs::create_dir_all(&a.quarantine).unwrap();
    fs::write(&held, b"our frame").unwrap();
    (home, held)
}

// ------------------------------------------------------------ R2 / D4 -----

/// R2. An old list names the frame and its sidecar but records nothing
/// about which file they are. The frame was moved aside and someone else's
/// file has its name at home. The undo used to take that file for the
/// frame "already back", carry the sidecar beside it and close the row.
#[test]
fn old_manifest_without_identity_does_not_guess_foreign_frame_is_home() {
    let a = archive();
    let (home, held) = held_frame(&a);
    let (side, held_side) = (home.with_extension("xmp"), held.with_extension("xmp"));
    fs::write(&held_side, b"our edits").unwrap();
    let id = row(&a, &home, &held, &[], JournalStatus::Done);
    a.db.conn
        .execute(
            "UPDATE journal SET manifest=?1 WHERE id=?2",
            rusqlite::params![
                format!(
                    r#"[{{"src":{:?},"dst":{:?}}},{{"src":{:?},"dst":{:?}}}]"#,
                    home.display().to_string(),
                    held.display().to_string(),
                    side.display().to_string(),
                    held_side.display().to_string()
                ),
                id
            ],
        )
        .unwrap();
    fs::rename(&held, a.dir.join("our-frame-saved.arw")).unwrap();
    fs::write(&home, b"foreign frame").unwrap();

    let r = crate::undo(&a.db, id);

    assert!(r.is_err(), "unproved identity taken for ours: {r:?}");
    assert_eq!(fs::read(&home).unwrap(), b"foreign frame");
    assert_eq!(
        fs::read(&held_side).unwrap(),
        b"our edits",
        "sidecar left quarantine"
    );
    assert!(fs::symlink_metadata(&side).is_err());
    assert_eq!(journal_row(&a.db, id).0, "done");
    let shown = format!("{:#}", r.unwrap_err());
    assert!(shown.contains(&home.display().to_string()), "{shown}");
}

/// D4. The row knows which file it put in quarantine. Someone moved that
/// file aside and put another at its quarantine name: that one is not
/// carried into the archive.
#[test]
fn a_recorded_held_identity_is_checked_before_undo() {
    let a = archive();
    let (home, held) = held_frame(&a);
    let id = row(
        &a,
        &home,
        &held,
        &[recorded(&home, &held)],
        JournalStatus::Done,
    );
    fs::rename(&held, a.dir.join("retained-own.arw")).unwrap();
    fs::write(&held, b"foreign frame").unwrap();

    let r = crate::undo(&a.db, id);

    assert!(r.is_err(), "{r:?}");
    assert_eq!(fs::read(&held).unwrap(), b"foreign frame");
    assert!(fs::symlink_metadata(&home).is_err(), "a stranger came home");
    assert_eq!(journal_row(&a.db, id).0, "done");
}

/// A dangling symlink where the frame was held is not the frame, and a
/// dangling symlink at home is not "nothing there".
#[cfg(unix)]
#[test]
fn a_dangling_entry_is_not_missing() {
    let a = archive();
    let (home, held) = held_frame(&a);
    let id = row(
        &a,
        &home,
        &held,
        &[recorded(&home, &held)],
        JournalStatus::Done,
    );
    fs::rename(&held, a.dir.join("retained-own.arw")).unwrap();
    std::os::unix::fs::symlink(a.dir.join("nowhere"), &held).unwrap();
    assert!(crate::undo(&a.db, id).is_err());
    assert!(fs::symlink_metadata(&held)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(&home).is_err(), "a link came home");

    // Pending: the frame is held, a dangling link has the home name.
    let b = archive();
    let (home, held) = held_frame(&b);
    let id = row(
        &b,
        &home,
        &held,
        &[recorded(&home, &held)],
        JournalStatus::Pending,
    );
    std::os::unix::fs::symlink(b.dir.join("nowhere"), &home).unwrap();
    let read = crate::reconcile(&b.db, id).unwrap();
    assert!(
        !matches!(
            read[0].standing,
            crate::Standing::Moved | crate::Standing::Home
        ),
        "{:?}",
        read[0].standing
    );
    assert!(crate::reconcile_undo(&b.db, id).is_err());
    assert_eq!(fs::read(&held).unwrap(), b"our frame");
    assert_eq!(journal_row(&b.db, id).0, "pending");
}

/// What cannot be read is not absent: a home folder the tool may not look
/// into is not an empty place to bring a file back to.
#[cfg(unix)]
#[test]
fn unreadable_is_not_absent() {
    use std::os::unix::fs::PermissionsExt;
    let a = archive();
    let sub = a.dir.join("locked");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir_all(&a.quarantine).unwrap();
    let home = sub.join("frame.arw");
    let held = a.quarantine.join("frame.arw");
    fs::write(&held, b"our frame").unwrap();
    let id = row(
        &a,
        &home,
        &held,
        &[recorded(&home, &held)],
        JournalStatus::Pending,
    );
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::symlink_metadata(&home).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
        // Root reads through permissions: nothing is unreadable here.
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("setup not achievable: permissions do not hide entries from this user");
        return;
    }
    let read = crate::reconcile(&a.db, id);
    let r = crate::reconcile_undo(&a.db, id);
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    if let Ok(read) = read {
        assert!(
            !matches!(
                read[0].standing,
                crate::Standing::Moved | crate::Standing::Home
            ),
            "{:?}",
            read[0].standing
        );
    }
    assert!(r.is_err());
    assert_eq!(fs::read(&held).unwrap(), b"our frame");
    assert_eq!(journal_row(&a.db, id).0, "pending");
}

/// Old rows have no evidence of which file is theirs. Whatever sits at
/// home is not proof the operation came back: neither an undo nor a
/// recovery may close such a row on a name.
#[test]
fn legacy_unknown_identity_never_closes_recovery() {
    // A pending row from before the journal held a list: something with
    // the photograph's name is at home, nothing is held.
    let a = archive();
    let home = a.dir.join("frame.arw");
    let held = a.quarantine.join("frame.arw");
    fs::write(&home, b"whose?").unwrap();
    let id = row(&a, &home, &held, &[], JournalStatus::Pending);
    assert!(crate::reconcile_undo(&a.db, id).is_err());
    assert_eq!(journal_row(&a.db, id).0, "pending");
    assert_eq!(fs::read(&home).unwrap(), b"whose?");

    // A done row with an old list: both files have names at home and
    // nothing is held. It is not marked undone on that.
    let b = archive();
    let home = b.dir.join("frame.arw");
    let held = b.quarantine.join("frame.arw");
    fs::write(&home, b"whose?").unwrap();
    fs::write(home.with_extension("xmp"), b"whose edits?").unwrap();
    let list = [
        pair(&home, &held),
        pair(&home.with_extension("xmp"), &held.with_extension("xmp")),
    ];
    let id = row(&b, &home, &held, &list, JournalStatus::Done);
    assert!(crate::undo(&b.db, id).is_err());
    assert!(
        crate::undo(&b.db, id).is_err(),
        "a retry turned a guess into a fact"
    );
    assert_eq!(journal_row(&b.db, id).0, "done");
}

/// D5. A pending row knows which file it moved. That file was taken aside
/// and another one put at home: home being taken is not the frame home.
#[test]
fn reconcile_uses_identity_instead_of_home_name() {
    let a = archive();
    let (home, held) = held_frame(&a);
    let id = row(
        &a,
        &home,
        &held,
        &[recorded(&home, &held)],
        JournalStatus::Pending,
    );
    fs::rename(&held, a.dir.join("retained-own.arw")).unwrap();
    fs::write(&home, b"foreign frame").unwrap();

    let r = crate::reconcile_undo(&a.db, id);

    assert!(r.is_err(), "{r:?}");
    assert_eq!(journal_row(&a.db, id).0, "pending");
    assert_eq!(fs::read(&home).unwrap(), b"foreign frame");
}

/// An interrupted forward move: the frame went, the process stopped before
/// the row was finished. Recovery works from the evidence recorded before
/// the move, so the frame comes back, and a file swapped in at its
/// quarantine name meanwhile does not.
#[test]
fn retry_after_crash_uses_persisted_object_evidence() {
    for swapped in [false, true] {
        let a = archive();
        let photo = a.dir.join("frame.arw");
        fs::write(&photo, b"our frame").unwrap();
        let c = candidate(&a.db, a.run, &photo);
        // The process "dies" right after the rename: nothing after it is
        // written.
        a.db.conn
            .execute_batch(
                "CREATE TRIGGER crash BEFORE UPDATE OF status ON journal
                 BEGIN SELECT RAISE(ABORT, 'process killed'); END;",
            )
            .unwrap();
        let _ = crate::files::quarantine_file(&a.db, a.run, &c, None);
        a.db.conn.execute_batch("DROP TRIGGER crash").unwrap();
        let id = last_journal_id(&a.db);
        assert_eq!(journal_row(&a.db, id).0, "pending");
        let held = a.quarantine.join("frame.arw");
        assert_eq!(fs::read(&held).unwrap(), b"our frame");
        if swapped {
            fs::rename(&held, a.dir.join("retained-own.arw")).unwrap();
            fs::write(&held, b"foreign frame").unwrap();
        }

        let r = crate::reconcile_undo(&a.db, id);

        if swapped {
            assert!(r.is_err(), "a stranger was recovered: {r:?}");
            assert!(fs::symlink_metadata(&photo).is_err());
            assert_eq!(journal_row(&a.db, id).0, "pending");
        } else {
            r.unwrap();
            assert_eq!(fs::read(&photo).unwrap(), b"our frame");
            assert_eq!(journal_row(&a.db, id).0, "undone");
        }
    }
}

/// The frame came home, its sidecar did not. Then the file at home was
/// rewritten in place: same device and inode, different object state —
/// what an inode handed out again looks like to `dev:ino`. The retry does
/// not accept it as the frame the undo brought back.
#[test]
fn identity_reuse_is_not_silently_accepted() {
    let a = archive();
    let photo = a.dir.join("frame.arw");
    fs::write(&photo, b"our frame").unwrap();
    fs::write(photo.with_extension("xmp"), b"our edits").unwrap();
    let c = candidate(&a.db, a.run, &photo);
    crate::apply(&a.db, a.run, &[c], None).unwrap();
    let id = last_journal_id(&a.db);
    fs::write(photo.with_extension("xmp"), b"foreign edits").unwrap();
    assert!(crate::undo(&a.db, id).is_err());
    assert_eq!(fs::read(&photo).unwrap(), b"our frame");

    // Same inode, other content and size.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&photo, b"a different photograph entirely").unwrap();
    fs::remove_file(photo.with_extension("xmp")).unwrap();

    assert!(
        crate::undo(&a.db, id).is_err(),
        "continuity taken on dev:ino"
    );
    assert_eq!(journal_row(&a.db, id).0, "done");
    assert!(
        a.quarantine.join("frame.xmp").exists(),
        "sidecar left with an unproved frame"
    );
}

// --------------------------------------------------------- R3 / D6 -------

/// R3. The same refusal twice: the second attempt adds itself to the
/// history instead of replacing it.
#[test]
fn repeated_retry_keeps_the_original_note() {
    let a = archive();
    let (home, held) = held_frame(&a);
    fs::write(a.quarantine.join("frame.xmp"), b"our edits").unwrap();
    let id = row(&a, &home, &held, &[], JournalStatus::Done);
    fs::write(home.with_extension("xmp"), b"foreign edits").unwrap();
    assert!(crate::undo(&a.db, id).is_err());
    assert!(note(&a, id).contains("prior recovery evidence"));
    assert!(crate::undo(&a.db, id).is_err());
    assert!(
        note(&a, id).contains("prior recovery evidence"),
        "a repeated refusal erased history: {}",
        note(&a, id)
    );
}

/// Every attempt is its own event: identical ones, and one whose words are
/// contained in an earlier one, are all kept, in order.
#[test]
fn repeated_and_distinct_refusals_preserve_all_events() {
    let a = archive();
    let (home, held) = held_frame(&a);
    let held_side = a.quarantine.join("frame.xmp");
    fs::write(&held_side, b"our edits").unwrap();
    let list = [
        recorded(&home, &held),
        recorded(&home.with_extension("xmp"), &held_side),
    ];
    let id = row(&a, &home, &held, &list, JournalStatus::Done);
    let side = home.with_extension("xmp");
    fs::write(&side, b"foreign edits").unwrap();
    assert!(crate::undo(&a.db, id).is_err());
    assert!(crate::undo(&a.db, id).is_err());
    let side_name = side.display().to_string();
    let n = note(&a, id);
    assert!(n.starts_with("prior recovery evidence"), "{n}");
    assert_eq!(n.matches(&side_name).count(), 2, "{n}");

    fs::remove_file(&side).unwrap();
    crate::undo(&a.db, id).unwrap();
    let n = note(&a, id);
    assert!(n.starts_with("prior recovery evidence"), "{n}");
    assert_eq!(n.matches(&side_name).count(), 2, "{n}");
    assert_eq!(journal_row(&a.db, id).0, "undone");
}

/// D6a. Both paths taken: the refusal is added to what the row said.
#[test]
fn reconcile_ambiguity_preserves_prior_evidence() {
    let a = archive();
    let (home, held) = held_frame(&a);
    let id = row(
        &a,
        &home,
        &held,
        &[recorded(&home, &held)],
        JournalStatus::Pending,
    );
    fs::write(&home, b"foreign frame").unwrap();
    assert!(crate::reconcile_undo(&a.db, id).is_err());
    assert!(
        note(&a, id).contains("prior recovery evidence"),
        "{}",
        note(&a, id)
    );
    assert!(
        note(&a, id).contains(&home.display().to_string()),
        "{}",
        note(&a, id)
    );
}

/// D6b. A successful recovery adds its own line and keeps the history.
#[test]
fn reconcile_success_preserves_prior_evidence() {
    let a = archive();
    let (home, held) = held_frame(&a);
    let id = row(
        &a,
        &home,
        &held,
        &[recorded(&home, &held)],
        JournalStatus::Pending,
    );
    crate::reconcile_undo(&a.db, id).unwrap();
    assert!(
        note(&a, id).contains("prior recovery evidence"),
        "{}",
        note(&a, id)
    );
    assert_eq!(journal_row(&a.db, id).0, "undone");
}

/// The frame's way home is taken, and the journal cannot record that. The
/// caller hears both: the refusal, and that it is not written down.
#[test]
fn a_note_write_failure_is_reported_with_the_original_refusal() {
    let a = archive();
    let (home, held) = held_frame(&a);
    let id = row(
        &a,
        &home,
        &held,
        &[recorded(&home, &held)],
        JournalStatus::Done,
    );
    let _hook = race_at(home.clone(), Stranger::File);
    a.db.conn
        .execute_batch(
            "CREATE TRIGGER broken BEFORE UPDATE OF note ON journal
             BEGIN SELECT RAISE(ABORT, 'injected journal failure'); END;",
        )
        .unwrap();
    let r = crate::undo(&a.db, id);
    a.db.conn.execute_batch("DROP TRIGGER broken").unwrap();
    let shown = format!("{:#}", r.unwrap_err());
    assert!(shown.contains(&home.display().to_string()), "{shown}");
    assert!(shown.contains("injected journal failure"), "{shown}");
    assert_eq!(fs::read(&held).unwrap(), b"our frame");
    intact(&home, Stranger::File);
}

// --------------------------------------------------------- D7 / R4 -------

/// D7. The frame (9 bytes) moved, its sidecar (15) did not: quarantine
/// holds 9 bytes, and that is what the journal and the totals say.
#[test]
fn a_refused_sidecar_does_not_inflate_quarantine_bytes() {
    let a = archive();
    let home = a.dir.join("frame.arw");
    fs::write(&home, b"our frame").unwrap();
    fs::write(home.with_extension("xmp"), b"edits not moved").unwrap();
    let c = candidate(&a.db, a.run, &home);
    fs::create_dir_all(&a.quarantine).unwrap();
    fs::write(a.quarantine.join("frame.xmp"), b"foreign edits").unwrap();

    crate::apply(&a.db, a.run, &[c], None).unwrap();

    let t = crate::quarantined_totals(&a.db).unwrap();
    assert_eq!(t.bytes, 9, "{}", t.summary());
    assert_eq!(t.files, 1, "{}", t.summary());
}

/// A reorganisation stopped in its litter sweep: the frame, its sidecar
/// and the first service file moved; the second service file met the
/// volume. All three moves are in what the stop reports.
#[test]
fn partial_outcomes_include_moved_sidecars_and_litter() {
    let a = archive();
    let old = a.dir.join("old");
    fs::create_dir_all(&old).unwrap();
    let src = old.join("frame.jpg");
    fs::write(&src, b"702 bytes? no: 21 bytes").unwrap();
    fs::write(old.join("frame.xmp"), b"edits").unwrap();
    fs::write(old.join(".DS_Store"), b"finder").unwrap();
    fs::write(old.join("._zz"), b"fork").unwrap();
    let dest = a.dir.join("new");
    fs::create_dir_all(&dest).unwrap();
    let m = organized(&a, &src, &dest.join("frame.jpg"));
    // The second service file the sweep reaches meets the volume, in
    // whatever order the directory lists them.
    let mut litter = 0;
    let at = old.clone();
    let _refused = race::before_move(move |from, _| {
        let name = from.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if from.parent() == Some(at.as_path()) && (name == ".DS_Store" || name.starts_with("._")) {
            litter += 1;
            if litter == 2 {
                return Err(io::Error::from(io::ErrorKind::Unsupported));
            }
        }
        Ok(())
    });

    let e = crate::organize(&a.db, a.run, &[m]).unwrap_err();

    assert!(crate::is_no_exclusive_rename(&e));
    let shown = e.to_string();
    for part in [
        ["1 file", "1 файл"],
        ["1 companion", "1 спутник"],
        ["1 service file", "1 служебный файл"],
    ] {
        assert!(
            shown.contains(part[0]) || shown.contains(part[1]),
            "{part:?}: {shown}"
        );
    }
    assert!(dest.join("frame.xmp").exists());
    let left = [".DS_Store", "._zz"]
        .iter()
        .filter(|n| old.join(n).exists())
        .count();
    assert_eq!(left, 1, "one service file moved, one stayed");
}

/// Evidence that cannot establish continuity — a later version's proof, or
/// a birth time the volume does not report as recorded — proves nothing: the
/// undo and the recovery both refuse, the frame stays held, the row stays
/// open and the refusal names both places. (The Windows half of D4 is the
/// refusal in `check_exclusive_rename`; no proof is ever taken there.)
#[cfg(unix)]
#[test]
fn unsupported_identity_fails_closed() {
    for (status, label) in [
        (JournalStatus::Done, "done"),
        (JournalStatus::Pending, "pending"),
    ] {
        for kind in ["future version", "birth time unreadable"] {
            let a = archive();
            let (home, held) = held_frame(&a);
            let mut m = recorded(&home, &held);
            let proof = m.proof.as_mut().unwrap();
            match kind {
                "future version" => proof.v = pc_core::proof::VERSION + 1,
                _ => {
                    // A birth time this volume cannot report now (where it
                    // reports none), or not the one it reports.
                    proof.birth_ns = Some(proof.birth_ns.map_or(1, |b| b + 1));
                }
            }
            let id = row(&a, &home, &held, &[m], status);

            let r = if status == JournalStatus::Done {
                crate::undo(&a.db, id).map(|_| ())
            } else {
                crate::reconcile_undo(&a.db, id).map(|_| ())
            };

            let e = r.expect_err(&format!("{label}/{kind}: recovered on unreadable evidence"));
            let shown = format!("{e:#}");
            assert!(
                shown.contains(&held.display().to_string()),
                "{label}/{kind}: {shown}"
            );
            assert!(
                shown.contains(&home.display().to_string()),
                "{label}/{kind}: {shown}"
            );
            assert_eq!(fs::read(&held).unwrap(), b"our frame", "{label}/{kind}");
            assert!(fs::symlink_metadata(&home).is_err(), "{label}/{kind}");
            assert_eq!(journal_row(&a.db, id).0, label, "{label}/{kind}");
        }
    }
}

/// Windows (el-usdqi D4): no object identity is taken there, so recovery
/// never falls back to a path key — a frame held in quarantine is not
/// carried home on its name, and the row stays open. Compiled and run only
/// on Windows; not executed by the macOS/Linux gates.
#[cfg(windows)]
#[test]
fn windows_recovery_uses_file_identity_not_a_path_key() {
    for status in [JournalStatus::Done, JournalStatus::Pending] {
        let a = archive();
        let (home, held) = held_frame(&a);
        let manifest = [pc_db::Moved {
            src: home.display().to_string(),
            dst: held.display().to_string(),
            proof: None,
        }];
        let id = row(&a, &home, &held, &manifest, status);

        let r = if status == JournalStatus::Done {
            crate::undo(&a.db, id).map(|_| ())
        } else {
            crate::reconcile_undo(&a.db, id).map(|_| ())
        };

        assert!(r.is_err());
        assert_eq!(fs::read(&held).unwrap(), b"our frame");
        assert!(fs::symlink_metadata(&home).is_err());
    }
}
