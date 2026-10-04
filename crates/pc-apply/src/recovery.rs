//! One way of reading a journal entry against the disk, shared by every
//! recovery: an undo, its retry, the reconciliation of an interrupted run,
//! and the web's preview of either (el-usdqi, el-5vue3 §2).
//!
//! Each file of an entry has two places: `src`, where it lived, and `dst`,
//! where the operation put it. What sits at each is read without following
//! links, an unreadable place is not an empty one, and whatever is found is
//! compared with the evidence the operation recorded about the file before
//! it first moved ([`pc_core::proof`]). A file is treated as this entry's
//! only on that evidence — never because it has the right name, never
//! because an old list mentions the path. The answer is one [`Standing`] per
//! file, and the only thing ever moved is a file that is [`Standing::Moved`]:
//! held where the operation put it, nothing at home.
//!
//! Rows written before evidence was recorded can only ever be read as far
//! as their paths go. A file held at the exact quarantine path the journal
//! recorded may come back (that path was the operation's own); evidence is
//! taken from it before it moves, so that a retry can recognise it at home.
//! A file at home in such a row is never taken for the one that left: the
//! row stays open, the file stays in quarantine, and the refusal names both
//! paths (director decision, el-usdqi: no name-based recognition; the
//! per-file choice for such rows is follow-up el-14vx0).

use anyhow::{bail, Context, Result};
use pc_core::proof::{Proof, Verdict};
use pc_db::{BundleState, Db, JournalEntry, JournalStatus, Moved};
use std::fs;
use std::path::{Path, PathBuf};

use crate::{files, organize, rename_with_parents, stop_run, Route, Tally};

/// Where one file of an entry is, against its evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// At its source, and shown to be this file: never moved, or already
    /// brought back. Nothing to do.
    Home,
    /// Held where the operation put it, nothing at its source: it can come
    /// back.
    Moved,
    /// Something at both paths. The tool will not choose between two files.
    Both,
    /// At neither path. Something outside this tool has been here.
    Gone,
    /// Something is there that cannot be shown to be this file, or cannot
    /// be read at all. Nothing is moved on its account.
    Doubt(String),
}

#[derive(Debug, Clone)]
pub struct Item {
    pub src: String,
    pub dst: String,
    pub standing: Standing,
}

impl Item {
    /// Why this one keeps the entry open, in words that name both paths.
    pub fn why(&self) -> Option<String> {
        Some(match &self.standing {
            Standing::Home | Standing::Moved => return None,
            Standing::Both => pc_core::tf!(
                "{0} — файл есть и на исходном месте, и в карантине ({1}); выбирать между ними инструмент не будет",
                "{0} — there is a file at its source and in quarantine ({1}); the tool will not choose between them",
                self.src,
                self.dst
            ),
            Standing::Gone => pc_core::tf!(
                "{0} — файла нет ни на исходном месте, ни в карантине ({1})",
                "{0} — the file is neither at its source nor in quarantine ({1})",
                self.src,
                self.dst
            ),
            Standing::Doubt(why) => why.clone(),
        })
    }
}

/// What is at one path, against the evidence.
enum Spot {
    Absent,
    Proven,
    /// Something is there; the entry recorded nothing to compare it with.
    Unproven,
    /// Something is there and it is not the file, or cannot be shown to be.
    Other(&'static str),
    Unreadable(String),
}

fn spot(path: &str, proof: Option<&Proof>) -> Spot {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Spot::Absent,
        // Not "nothing there": a permission or I/O error, or a component
        // that is not a folder, says nothing about what is there.
        Err(e) => Spot::Unreadable(e.to_string()),
        Ok(md) => match proof {
            None => Spot::Unproven,
            Some(p) => match p.check(&md) {
                Verdict::Same => Spot::Proven,
                Verdict::Differs(w) | Verdict::Unprovable(w) => Spot::Other(w),
            },
        },
    }
}

/// The one classifier.
pub fn classify(m: &Moved) -> Standing {
    let held = spot(&m.dst, m.proof.as_ref());
    let home = spot(&m.src, m.proof.as_ref());
    match (held, home) {
        (Spot::Unreadable(e), _) => Standing::Doubt(pc_core::tf!(
            "{0} — место в карантине не прочитать: {1}",
            "{0} — its place in quarantine cannot be read: {1}",
            m.dst,
            e
        )),
        (_, Spot::Unreadable(e)) => Standing::Doubt(pc_core::tf!(
            "{0} — исходное место не прочитать: {1}; файл остаётся в карантине: {2}",
            "{0} — its original place cannot be read: {1}; the file stays in quarantine: {2}",
            m.src,
            e,
            m.dst
        )),
        (Spot::Other(why), _) => Standing::Doubt(pc_core::tf!(
            "в карантине по пути {0} лежит не тот файл, что перенесла эта операция ({1}); он не переносится в {2}",
            "in quarantine at {0} is not the file this operation moved there ({1}); it is not moved to {2}",
            m.dst,
            why,
            m.src
        )),
        (Spot::Proven | Spot::Unproven, Spot::Absent) => Standing::Moved,
        (Spot::Proven | Spot::Unproven, _) => Standing::Both,
        (Spot::Absent, Spot::Proven) => Standing::Home,
        (Spot::Absent, Spot::Unproven) => Standing::Doubt(pc_core::tf!(
            "на месте {0} лежит файл, и ничто не доказывает, что это тот, что ушёл в карантин ({1}); запись остаётся открытой",
            "at {0} there is a file, and nothing proves it is the one that went to quarantine ({1}); the entry stays open",
            m.src,
            m.dst
        )),
        (Spot::Absent, Spot::Other(why)) => Standing::Doubt(pc_core::tf!(
            "на месте {0} лежит другой файл, не тот, что ушёл в карантин ({1})",
            "at {0} there is another file, not the one that went to quarantine ({1})",
            m.src,
            why
        )),
        (Spot::Absent, Spot::Absent) => Standing::Gone,
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Refuse an entry whose recorded list is there but cannot be read by this
/// version (el-1y8uo B2). Its evidence is unknown, not absent: reading it as
/// a row from before lists were kept would let name-based legacy discovery
/// and adoption act on whatever now bears the recorded paths. Nothing is
/// moved, the record is not rewritten, and the refusal names both paths and
/// the record itself.
pub(crate) fn readable(entry: &JournalEntry) -> Result<()> {
    let Some(raw) = &entry.manifest_unreadable else {
        return Ok(());
    };
    bail!(
        "{}",
        pc_core::tf!(
            "запись {0} ({1} -> {2}): список перенесённого записан в виде, который эта версия не читает (запись более новой версии или повреждение); ничего не перемещается, запись не меняется. Записано: {3}",
            "entry {0} ({1} -> {2}): its list of what moved is recorded in a form this version cannot read (a later version's record, or damage); nothing is moved and the record is left as it is. Recorded: {3}",
            entry.id,
            entry.src,
            entry.dst.as_deref().unwrap_or("?"),
            raw
        )
    )
}

/// The list an entry is walked back from: what it wrote down, or, for a row
/// from before the journal held one, the photograph and whatever carries its
/// name beside it in quarantine — exactly what the older versions moved.
fn list_of(entry: &JournalEntry) -> Result<Vec<Moved>> {
    readable(entry)?;
    if !entry.manifest.is_empty() {
        return Ok(entry.manifest.clone());
    }
    let dst = entry.dst.clone().context(pc_core::tr!(
        "в записи нет пути назначения",
        "the entry has no destination path"
    ))?;
    let (dst_path, src_path) = (PathBuf::from(&dst), PathBuf::from(&entry.src));
    let new_stem = organize::stem_of(&name_of(&dst_path)).to_string();
    let old_stem = organize::stem_of(&name_of(&src_path)).to_string();
    let mut list = vec![Moved::new(entry.src.clone(), dst)];
    if entry.op != "quarantine" {
        for side in files::companions(&dst_path) {
            if let Some(name) = side.file_name().and_then(|s| s.to_str()) {
                let back =
                    src_path.with_file_name(organize::sidecar_name(name, &new_stem, &old_stem));
                list.push(Moved::new(back.to_string_lossy(), side.to_string_lossy()));
            }
        }
    }
    Ok(list)
}

/// What an undo of `entry` would find, file by file. Reads only: the web's
/// preview and the undo itself ask this same question.
pub fn undo_preview(entry: &JournalEntry) -> Result<Vec<Item>> {
    Ok(list_of(entry)?
        .iter()
        .map(|m| Item {
            src: m.src.clone(),
            dst: m.dst.clone(),
            standing: classify(m),
        })
        .collect())
}

/// Add `text` to the entry's history. If the journal cannot take it, the
/// caller's error says so as well — a refusal is never reported as written
/// down when it was not.
fn told(db: &Db, id: i64, phase: &str, kind: &str, text: &str, e: anyhow::Error) -> anyhow::Error {
    match db.journal_event(id, phase, kind, text, None) {
        Ok(()) => e,
        Err(pe) => e.context(pc_core::tf!(
            "журнал не принял запись об этом ({0}); запись {1} не дополнена",
            "the journal did not take the record of this ({0}); entry {1} was not updated",
            format!("{pe:#}"),
            id
        )),
    }
}

pub fn undo(db: &Db, journal_id: i64) -> Result<Tally> {
    let entry = db.journal_entry(journal_id)?.with_context(|| {
        pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
    })?;
    if entry.status != JournalStatus::Done {
        bail!(
            "{}",
            pc_core::tf!(
                "запись {0} в состоянии «{1}», откат невозможен",
                "entry {0} is “{1}”; it cannot be undone",
                journal_id,
                entry.status.as_str()
            )
        );
    }
    let dst = entry.dst.clone().context(pc_core::tr!(
        "в записи нет пути назначения",
        "the entry has no destination path"
    ))?;
    let mut list = match list_of(&entry) {
        Ok(list) => list,
        Err(e) => {
            let why = format!("{e:#}");
            return Err(told(db, journal_id, "undo", "refused", &why, e));
        }
    };

    let mut done = Tally::default();
    let mut came: Vec<Moved> = Vec::new();
    // The photograph first: if it cannot come back, nothing should move.
    let frame = list
        .iter()
        .find(|m| m.src == entry.src)
        .cloned()
        .unwrap_or_else(|| Moved::new(entry.src.clone(), dst.clone()));
    let item = Item {
        src: frame.src.clone(),
        dst: frame.dst.clone(),
        standing: classify(&frame),
    };
    if matches!(item.standing, Standing::Moved | Standing::Home) {
        // A row written without evidence: the files held at its own
        // recorded quarantine paths are what it has. Their evidence is taken
        // now, before they move, and written down — so a retry tells the one
        // this undo brings home from a stranger that later takes its name.
        // Only once the photograph is free to come back: a row refused here
        // stays exactly as it was.
        adopt_held_evidence(db, journal_id, &mut list)?;
    }
    match &item.standing {
        Standing::Moved => {
            if let Err(e) = rename_with_parents(Path::new(&frame.dst), Path::new(&frame.src)) {
                let why = pc_core::tf!(
                    "откат: не вернулось {0} — {1}",
                    "undo: did not come back — {0} — {1}",
                    frame.dst,
                    e
                );
                return Err(told(db, journal_id, "undo", "refused", &why, e));
            }
            done.files_back += 1;
            came.push(frame.clone());
        }
        Standing::Home => {}
        _ => {
            let why = pc_core::tf!(
                "откат: снимок остаётся в карантине — {0}",
                "undo: the photograph stays in quarantine — {0}",
                item.why().unwrap_or_default()
            );
            return Err(told(
                db,
                journal_id,
                "undo",
                "refused",
                &why,
                anyhow::anyhow!("{why}"),
            ));
        }
    }

    let mut failed: Vec<String> = Vec::new();
    let mut stop: Option<anyhow::Error> = None;
    let mut rest = list.iter().filter(|m| m.src != entry.src);
    for m in rest.by_ref() {
        let item = Item {
            src: m.src.clone(),
            dst: m.dst.clone(),
            standing: classify(m),
        };
        match &item.standing {
            Standing::Home => continue,
            Standing::Moved => {}
            _ => {
                failed.push(item.why().unwrap_or_default());
                continue;
            }
        }
        match rename_with_parents(Path::new(&m.dst), Path::new(&m.src)) {
            Ok(()) => {
                done.files_back += 1;
                came.push(m.clone());
            }
            Err(e) if crate::is_no_exclusive_rename(&e) => {
                // The volume: nothing more is tried on it.
                failed.push(format!("{} — {e}", m.dst));
                stop = Some(e);
                break;
            }
            Err(e) => failed.push(format!("{} — {e}", m.dst)),
        }
    }
    if stop.is_some() {
        for left in rest {
            failed.push(format!("{} — {}", left.dst, crate::not_tried()));
        }
    }

    // What did come back, the index should say is back: the photograph is in
    // the archive whether or not its sidecar managed to follow.
    //
    // Which row this concerns is decided by path, not by the id the entry was
    // written with. After a reset those ids belong to other files, and an
    // entry from an older database would otherwise reach into the new index
    // and change a stranger.
    //
    // Files are already back by now: a database that refuses any of what
    // follows does not undo that. The caller hears what came back, typed,
    // and that the journal did not record it (el-1y8uo B3); the entry keeps
    // its state, and asking again finishes it by the same evidence.
    let unrecorded = |e: anyhow::Error, done: &Tally| -> anyhow::Error {
        let e = e.context(pc_core::tf!(
            "файлы вернулись, но журнал не записал откат записи {0}; повторный откат завершит его",
            "the files came back, but the journal did not record the undo of entry {0}; asking again finishes it",
            journal_id
        ));
        stop_run(e, done, Route::Restore, Vec::new())
    };
    let indexed = (|| -> Result<()> {
        match entry.op.as_str() {
            "quarantine" => {
                if let Some(id) = db.bundle_id_at(&entry.src)? {
                    db.set_bundle_state(id, BundleState::Present)?;
                }
            }
            "quarantine-file" => {
                if let Some(id) = db.file_id_at(&entry.src)? {
                    db.set_file_state(id, "present")?;
                }
            }
            // The reorganisation moved the file, so the index knows it by
            // where it was moved to.
            "organize" => {
                if let Some(id) = db.file_id_at(&dst)? {
                    db.set_file_path(id, &entry.src, &name_of(Path::new(&entry.src)))?;
                }
            }
            _ => {}
        }
        Ok(())
    })();
    if let Err(e) = indexed {
        if !failed.is_empty() {
            done.entries_partial += 1;
        }
        return Err(unrecorded(e, &done));
    }
    if !failed.is_empty() {
        // Half an undo is not an undo. Marking the entry `undone` would close
        // the only door back to what stayed behind. It stays `done`, the
        // reason is added to its history, and asking again carries on from
        // where this stopped.
        let why = pc_core::tf!(
            "откат: не вернулось {0}",
            "undo: did not come back — {0}",
            failed.join("; ")
        );
        done.entries_partial += 1;
        let e = stop.unwrap_or_else(|| anyhow::anyhow!("{why}"));
        let e = told(db, journal_id, "undo", "partial", &why, e);
        return Err(stop_run(e, &done, Route::Restore, Vec::new()));
    }
    // The state and the event that says why, together or not at all.
    let closed = db.journal_close(
        journal_id,
        JournalStatus::Undone,
        &pc_db::Event {
            text: pc_core::tr!("откат выполнен", "undone"),
            moved: &came,
            ..pc_db::Event::new("undo", "done")
        },
    );
    if let Err(e) = closed {
        return Err(unrecorded(e, &done));
    }
    done.entries_back += 1;
    Ok(done)
}

/// Read a `pending` entry against the disk.
///
/// The journal is written before the disk is touched and finished
/// afterwards, so a killed process leaves a `pending` row: the list of what
/// it meant to move, the evidence of each file, and no word on how far it
/// got. This says, for every file, where it is.
pub fn reconcile(db: &Db, journal_id: i64) -> Result<Vec<Item>> {
    let entry = db.journal_entry(journal_id)?.with_context(|| {
        pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
    })?;
    if entry.status != JournalStatus::Pending {
        bail!(
            "{}",
            pc_core::tf!(
                "запись {0} в состоянии «{1}»: сверять нечего",
                "entry {0} is “{1}”: there is nothing to reconcile",
                journal_id,
                entry.status.as_str()
            )
        );
    }
    // A purge is not reversible and never was: the bytes it removed are gone,
    // and an interrupted one leaves nothing to carry back.
    if entry.op.contains("purge") {
        bail!(
            "{}",
            pc_core::tr!(
                "прерванное окончательное удаление не восстанавливается: проверьте свою резервную копию",
                "an interrupted permanent deletion cannot be undone: check your own backup"
            )
        );
    }
    let list = pending_list(&entry)?;
    Ok(list
        .iter()
        .map(|m| Item {
            src: m.src.clone(),
            dst: m.dst.clone(),
            standing: classify(m),
        })
        .collect())
}

/// The list an interrupted entry is reconciled from.
fn pending_list(entry: &JournalEntry) -> Result<Vec<Moved>> {
    readable(entry)?;
    Ok(if entry.manifest.is_empty() {
        // Written before the journal held a list. One pair is all it knows.
        let dst = entry.dst.clone().context(pc_core::tr!(
            "в записи нет пути назначения",
            "the entry has no destination path"
        ))?;
        vec![Moved::new(entry.src.clone(), dst)]
    } else {
        entry.manifest.clone()
    })
}

/// A row written without evidence, about to be walked back: the files held
/// at its own recorded quarantine paths are what it has. Their evidence is
/// taken now and written down *before* anything moves (el-1y8uo B4), so a
/// retry after a partial return recognises what came home by proof — never
/// by its name. Rows that recorded evidence are left exactly as they are.
fn adopt_held_evidence(db: &Db, journal_id: i64, list: &mut [Moved]) -> Result<()> {
    let mut adopted = false;
    for m in list.iter_mut() {
        if m.proof.is_none() {
            if let Ok(md) = fs::symlink_metadata(&m.dst) {
                m.proof = Proof::of(&md);
                adopted |= m.proof.is_some();
            }
        }
    }
    if adopted {
        db.journal_record_manifest(journal_id, list)?;
    }
    Ok(())
}

/// What a recovery brought back.
#[derive(Debug, Clone)]
pub struct Reconciled {
    pub items: Vec<Item>,
    pub done: Tally,
}

/// Bring back what an interrupted operation moved, and close its entry.
///
/// Only when every file of it is accounted for: at home and shown to be
/// this one, or held and free to come back. Anything else leaves the entry
/// `pending`, which is what it is, with the reason added to its history.
pub fn reconcile_undo(db: &Db, journal_id: i64) -> Result<Reconciled> {
    if let Some(entry) = db.journal_entry(journal_id)? {
        if let Err(e) = readable(&entry) {
            let why = format!("{e:#}");
            return Err(told(db, journal_id, "reconcile", "refused", &why, e));
        }
    }
    let items = reconcile(db, journal_id)?;
    let unclear: Vec<String> = items.iter().filter_map(Item::why).collect();
    if !unclear.is_empty() {
        let why = pc_core::tf!(
            "сверка: ничего не перенесено — {0}",
            "reconcile: nothing was moved — {0}",
            unclear.join("; ")
        );
        let e = anyhow::anyhow!("{why}");
        return Err(told(db, journal_id, "reconcile", "refused", &why, e));
    }
    {
        let entry = db.journal_entry(journal_id)?.with_context(|| {
            pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
        })?;
        let mut list = pending_list(&entry)?;
        adopt_held_evidence(db, journal_id, &mut list)?;
    }
    let back: Vec<&Item> = items
        .iter()
        .filter(|i| i.standing == Standing::Moved)
        .collect();
    let mut done = Tally::default();
    let mut came: Vec<&str> = Vec::new();
    let mut at: Option<&Item> = None;
    let carried = crate::check_all(back.iter().map(|i| Path::new(&i.src))).and_then(|()| {
        for item in &back {
            at = Some(item);
            rename_with_parents(Path::new(&item.dst), Path::new(&item.src))?;
            came.push(&item.src);
            done.files_back += 1;
        }
        Ok(())
    });
    if let Err(e) = carried {
        let mut why = match at {
            Some(i) => pc_core::tf!(
                "сверка: не вернулось {0} — {1}",
                "reconcile: did not come back — {0} — {1}",
                i.src,
                e
            ),
            None => pc_core::tf!("сверка: отказ — {0}", "reconcile: refused — {0}", e),
        };
        if !came.is_empty() {
            why.push_str(&pc_core::tf!(
                "; уже вернулось: {0}",
                "; already back: {0}",
                came.join(", ")
            ));
            done.entries_partial += 1;
        }
        let e = told(db, journal_id, "reconcile", "partial", &why, e);
        return Err(stop_run(e, &done, Route::Restore, Vec::new()));
    }
    // Files are back by now; a database that refuses what follows does not
    // undo that (el-1y8uo B3). The entry stays pending with its evidence, and
    // reconciling it again finds them at home by proof and closes it.
    let returned: Vec<Moved> = back
        .iter()
        .map(|i| Moved::new(i.src.clone(), i.dst.clone()))
        .collect();
    let recorded = (|| -> Result<()> {
        let entry = db.journal_entry(journal_id)?.with_context(|| {
            pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
        })?;
        if entry.op == "quarantine-file" {
            if let Some(id) = db.file_id_at(&entry.src)? {
                db.set_file_state(id, "present")?;
            }
        } else if entry.op == "quarantine" {
            if let Some(id) = db.bundle_id_at(&entry.src)? {
                db.set_bundle_state(id, BundleState::Present)?;
            }
        }
        // Nothing of this operation is left in quarantine, which is what
        // `undone` says. It never finished, and the history keeps that fact.
        db.journal_close(
            journal_id,
            JournalStatus::Undone,
            &pc_db::Event {
                text: pc_core::tr!(
                    "прерванная операция сверена по манифесту и отменена",
                    "an interrupted operation was reconciled against its manifest and undone"
                ),
                moved: &returned,
                ..pc_db::Event::new("reconcile", "done")
            },
        )
    })();
    if let Err(e) = recorded {
        let e = e.context(pc_core::tf!(
            "файлы вернулись, но журнал не записал сверку записи {0}; она остаётся незавершённой, повторная сверка закроет её",
            "the files came back, but the journal did not record the reconciliation of entry {0}; it stays pending, and reconciling again closes it",
            journal_id
        ));
        let e = stop_run(e, &done, Route::Restore, Vec::new());
        return Err(crate::outcome::left_pending(e, journal_id, Route::Restore));
    }
    done.entries_back += 1;
    Ok(Reconciled { items, done })
}
