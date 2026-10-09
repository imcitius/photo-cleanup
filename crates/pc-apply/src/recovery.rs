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
//! as their paths go, and nothing proves which file is which: such a unit
//! is never moved back by an undo or a reconciliation, whether its place is
//! free or taken — it is kept in quarantine, and the refusal names where it
//! is and where it belongs, for a person to return by hand (user decision
//! 2026-10-10, el-14vx0; earlier versions adopted the evidence of a file
//! held at the recorded quarantine path and brought it back). What an undo
//! does, and what the person may choose when the place is taken, is
//! [`crate::conflict`]'s.

use anyhow::{bail, Context, Result};
use pc_core::proof::{Proof, Verdict};
use pc_db::{BundleState, Db, JournalEntry, JournalStatus, Moved};
use std::fs;
use std::path::{Path, PathBuf};

use crate::located::Way;
use crate::unit::{move_unit_checked, Member, Unit};
use crate::{files, organize, stop_run, Route, Tally};

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
    /// Where it is looked for: the recorded destination, or the place the
    /// journal last proved or saw it at (el-lvtmk R5).
    pub dst: String,
    pub standing: Standing,
    /// What the journal last recorded about its place, in words, when it
    /// recorded anything beyond the manifest.
    pub located: Option<String>,
}

impl Item {
    /// Why this one keeps the entry open, in words that name both paths.
    pub fn why(&self) -> Option<String> {
        let base = self.why_here()?;
        Some(match &self.located {
            Some(l) => format!("{base}; {l}"),
            None => base,
        })
    }

    fn why_here(&self) -> Option<String> {
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
pub(crate) enum Spot {
    Absent,
    Proven,
    /// Something is there; the entry recorded nothing to compare it with.
    Unproven,
    /// Something is there and it is not the file, or cannot be shown to be.
    Other(&'static str),
    Unreadable(String),
}

pub(crate) fn spot(path: &str, proof: Option<&Proof>) -> Spot {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Spot::Absent,
        // Not "nothing there": a permission or I/O error, or a component
        // that is not a folder, says nothing about what is there.
        Err(e) => Spot::Unreadable(e.to_string()),
        Ok(md) => match proof {
            None => Spot::Unproven,
            Some(p) => match verdict_at(path, p, &md) {
                Verdict::Same => Spot::Proven,
                Verdict::Differs(w) | Verdict::Unprovable(w) => Spot::Other(w),
            },
        },
    }
}

/// The evidence against what is at `path`, by [`Proof::verify`]: through
/// an open descriptor when the evidence records a hash, so the content is
/// compared too; a hash that cannot be read again is not a match.
fn verdict_at(path: &str, p: &Proof, md: &fs::Metadata) -> Verdict {
    if p.blake3.is_none() || !md.is_file() {
        return p.check(md);
    }
    #[cfg(unix)]
    {
        // Never follow a link, never wait on a pipe put in its place.
        let at = Path::new(path);
        let opened = match (at.parent(), at.file_name().and_then(|n| n.to_str())) {
            (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
                pc_core::anchored::Dir::open_following(parent)
                    .and_then(|d| d.open_file(name, false))
                    .ok()
            }
            _ => None,
        };
        match opened {
            Some(f) => p.check_file(&f),
            None => Verdict::Unprovable("its content can no longer be read"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        p.check(md)
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
pub(crate) fn list_of(entry: &JournalEntry) -> Result<Vec<Moved>> {
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

/// One item of an entry: as recorded, and where recovery looks for it.
///
/// The record is never rewritten. Where the operation later found the
/// object away from its recorded place — its folder moved under it, a
/// return that could not finish — a `located` event says where it was
/// proven or last seen (el-lvtmk R5), and recovery looks there instead of at
/// the recorded `dst`. Whatever is found there is still this entry's only on
/// its evidence ([`classify`]); the overlay is a place to look, never a
/// reason to act.
///
/// A return under a free name already under way (el-14vx0: `IMG.CR2` as
/// `IMG_1.CR2`, recorded before the rename) sets where its items are looked
/// for at home in the same way — and only once some item of it is proven
/// there by its evidence.
#[derive(Debug, Clone)]
pub(crate) struct Pair {
    pub(crate) rec: Moved,
    pub(crate) look: Moved,
    note: Option<String>,
    /// The object the operation last found is its own, changed since its
    /// check, and was kept where this says (user decision (c), point 4).
    pub(crate) changed: Option<String>,
}

impl Pair {
    /// This item comes back under a free name rather than its own.
    pub(crate) fn renamed(&self) -> bool {
        self.look.src != self.rec.src
    }
}

/// The free names an interrupted return under free names was taking, if it
/// had begun: the latest recorded attempt of which some item is proven — by
/// its evidence, never by its name — at its free name.
fn names_under_way(entry: &JournalEntry, list: &[Moved]) -> Vec<pc_db::ReturnedAs> {
    for attempt in entry.returned_as.iter().rev() {
        let begun = attempt.iter().any(|r| {
            list.iter()
                .find(|m| m.src == r.src && m.dst == r.dst)
                .and_then(|m| m.proof.as_ref())
                .is_some_and(|p| matches!(spot(&r.to, Some(p)), Spot::Proven))
        });
        if begun {
            return attempt.clone();
        }
    }
    Vec::new()
}

/// Every item of `list` as recovery reads it: [`Pair`].
pub(crate) fn pairs_of(entry: &JournalEntry, list: Vec<Moved>) -> Vec<Pair> {
    let under_way = names_under_way(entry, &list);
    list.into_iter()
        .map(|m| {
            let to = under_way
                .iter()
                .find(|r| r.src == m.src && r.dst == m.dst)
                .map(|r| r.to.clone());
            let mut p = pair_of(entry, m);
            if let Some(to) = to {
                p.look.src = to;
            }
            p
        })
        .collect()
}

fn pair_of(entry: &JournalEntry, rec: Moved) -> Pair {
    let l = entry.overlay_of(&rec);
    let mut look = rec.clone();
    if let Some(p) = l.and_then(|l| l.overlay()) {
        look.dst = p.to_string_lossy().into_owned();
    }
    let note = l.map(|l| {
        pc_core::tf!(
            "по последней записи журнала: {0}",
            "as the journal last recorded it: {0}",
            l.at.shown()
        )
    });
    let changed = l
        .filter(|l| l.held && l.role == "changed")
        .map(|l| l.at.shown());
    Pair {
        rec,
        look,
        note,
        changed,
    }
}

pub(crate) fn item_of(p: &Pair) -> Item {
    // A photograph that changed after its check and stayed with the tool is
    // never brought back by recovery: its evidence no longer says it is the
    // one that left, and a newer evidence is not a reason to act. It is told
    // where it is, for a person to settle (user decision (c), point 4).
    let standing = match &p.changed {
        Some(at) => Standing::Doubt(pc_core::tf!(
            "{0} — изменился после проверки и остался у инструмента; он не возвращается \
             автоматически, верните его вручную: {1}",
            "{0} — it changed after its check and stayed with the tool; it is not brought back \
             automatically, recover it by hand: {1}",
            p.rec.src,
            at
        )),
        None => classify(&p.look),
    };
    Item {
        src: p.look.src.clone(),
        dst: p.look.dst.clone(),
        standing,
        located: p.note.clone(),
    }
}

/// What an undo of `entry` would find, file by file. Reads only: the web's
/// preview and the undo itself ask this same question.
pub fn undo_preview(entry: &JournalEntry) -> Result<Vec<Item>> {
    Ok(pairs_of(entry, list_of(entry)?)
        .iter()
        .map(item_of)
        .collect())
}

/// Whether an undo of `entry` is offered: a finished operation that has not
/// been walked back. A refused one moved nothing that stayed moved, and an
/// unfinished one is reconciled instead (user decision (c)).
pub fn undo_offered(entry: &JournalEntry) -> bool {
    entry.status == JournalStatus::Done
}

/// Add `text` to the entry's history, with the places of what it concerns.
/// If the journal cannot take it, the caller's error says so as well — a
/// refusal is never reported as written down when it was not.
pub(crate) fn told(
    db: &Db,
    id: i64,
    phase: &str,
    kind: &str,
    text: &str,
    e: anyhow::Error,
) -> anyhow::Error {
    told_located(db, id, phase, kind, text, e, &[])
}

fn told_located(
    db: &Db,
    id: i64,
    phase: &str,
    kind: &str,
    text: &str,
    e: anyhow::Error,
    located: &[pc_db::Located],
) -> anyhow::Error {
    match db.journal_event_located(id, phase, kind, text, located) {
        Ok(()) => e,
        Err(pe) => e.context(pc_core::tf!(
            "журнал не принял запись об этом ({0}); запись {1} не дополнена",
            "the journal did not take the record of this ({0}); entry {1} was not updated",
            format!("{pe:#}"),
            id
        )),
    }
}

/// Every item of an entry with where it is — said whenever the entry is
/// refused, so that a person sees all of it, not only the part that failed.
pub(crate) fn listing(items: &[Item]) -> String {
    items
        .iter()
        .map(|i| match i.why() {
            Some(why) => why,
            None => match (&i.standing, &i.located) {
                (Standing::Home, _) => pc_core::tf!(
                    "{0} — на своём месте (проверено)",
                    "{0} — at its place (verified)",
                    i.src
                ),
                (_, Some(l)) => format!("{} — {l}", i.src),
                _ => pc_core::tf!(
                    "{0} — в {1} (проверено по записанным сведениям)",
                    "{0} — at {1} (verified by its recorded evidence)",
                    i.src,
                    i.dst
                ),
            },
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Walk an entry's items back as one unit (user decision (c)): only when
/// every item is proven — at home, or held with its home free — and then
/// all of them or none. Refused, the entry stays open with the reason and
/// every item's place added to its history; if the unit could not even be
/// put back where it was, the places proven after the last rename are
/// recorded and the run stops.
pub(crate) fn walk_back(
    db: &Db,
    id: i64,
    phase: &str,
    pairs: &[Pair],
    known: &[crate::Occupant],
) -> Result<(Vec<crate::bound::Arrived>, Vec<Moved>)> {
    let items: Vec<Item> = pairs.iter().map(item_of).collect();
    if items.iter().any(|i| i.why().is_some()) {
        let why = pc_core::tf!(
            "{0}: ничего не перенесено — кадр со спутниками возвращается только целиком, а не \
             всё доказано: {1}",
            "{0}: nothing was moved — the frame and its companions come back only together, \
             and not all of them are proven: {1}",
            phase,
            listing(&items)
        );
        return Err(told(
            db,
            id,
            phase,
            "refused",
            &why,
            anyhow::anyhow!("{why}"),
        ));
    }
    match move_back(pairs, known) {
        Back::Done(arrived, came) => Ok((arrived, came)),
        Back::Failed(unit) => Err(unit_failed(db, id, phase, &items, unit)),
    }
}

pub(crate) enum Back {
    /// Everything that was away came back: what arrived, held until it is
    /// recorded, and the pairs as they actually went.
    Done(Vec<crate::bound::Arrived>, Vec<Moved>),
    Failed(Unit),
}

/// Bring back, as one unit, every item of `pairs` that is away — to the
/// place its `look.src` names. Asked only once no item is in doubt; `known`
/// are the files the preview read beside it that are not part of it.
pub(crate) fn move_back(pairs: &[Pair], known: &[crate::Occupant]) -> Back {
    let moving: Vec<&Pair> = pairs
        .iter()
        .filter(|p| item_of(p).standing == Standing::Moved)
        .collect();
    if moving.is_empty() {
        return Back::Done(Vec::new(), Vec::new());
    }
    let members: Vec<Member> = moving
        .iter()
        .map(|p| Member::back(&p.rec, &p.look))
        .collect();
    // Something named as a companion of a member, appearing beside where
    // the unit was held while it moves, would be split from it (el-14vx0
    // R3-B1): the whole unit goes back and is refused.
    let origins: Vec<String> = moving.iter().map(|p| p.look.dst.clone()).collect();
    let left = || -> Option<String> {
        let new = crate::conflict::left_behind(&origins, known);
        (!new.is_empty()).then(|| crate::conflict::stranger_words(&new))
    };
    match move_unit_checked(&members, Way::Back, None, &[], &left) {
        Unit::Moved(arrived) => {
            // As it went: a return under a free name names the free name.
            let came = moving
                .iter()
                .map(|p| Moved {
                    src: p.look.src.clone(),
                    ..p.rec.clone()
                })
                .collect();
            Back::Done(arrived, came)
        }
        unit => Back::Failed(unit),
    }
}

/// The refusal of a unit that did not come back, added to the entry's
/// history with every item's place; a unit not put back whole stops the
/// run.
pub(crate) fn unit_failed(
    db: &Db,
    id: i64,
    phase: &str,
    items: &[Item],
    unit: Unit,
) -> anyhow::Error {
    let pb = match unit {
        Unit::Moved(_) => unreachable!("a unit that came back is not a failure"),
        Unit::Refused(e) => {
            let why = pc_core::tf!(
                "{0}: ничего не перенесено — {1}; где что: {2}",
                "{0}: nothing was moved — {1}; where things are: {2}",
                phase,
                e,
                listing(items)
            );
            let stop = crate::is_no_exclusive_rename(&e);
            let e = told(db, id, phase, "refused", &why, e);
            return if stop { e } else { e.context(why) };
        }
        Unit::PutBack(pb) => pb,
    };
    let why = format!("{phase}: {}", pb.why());
    let kind = if pb.kept { "kept" } else { "refused" };
    let located = pb.told.located.clone();
    let placed = pb.told.placed.clone();
    let stops = pb.stops();
    let folder = pb.told.folder_moved && !pb.kept;
    let e = if crate::is_no_exclusive_rename(&pb.error) {
        pb.error
    } else if folder {
        crate::FolderMoved {
            reason: why.clone(),
        }
        .into()
    } else {
        anyhow::anyhow!("{why}")
    };
    let e = told_located(db, id, phase, kind, &why, e, &located);
    let e = crate::outcome::with_placed(e, placed, Route::Restore);
    if stops {
        stop_run(e, &Tally::default(), Route::Restore, Vec::new())
    } else {
        e
    }
}

/// Walk a finished entry back: every file it moved, by its evidence, as one
/// unit. Where its original place is taken by another file, it stays in
/// quarantine — the default choice (el-14vx0) — and the refusal says where
/// it is and where it belongs; [`crate::undo_reviewed`] carries out the
/// other choice. A unit without evidence is kept, never moved.
pub fn undo(db: &Db, journal_id: i64) -> Result<Tally> {
    crate::conflict::undo_now(db, journal_id)
}

/// The finished entry `journal_id`, if it can be undone at all: offered,
/// with a destination and a list this version reads — a list it cannot
/// read is refused and written down.
pub(crate) fn undoable(db: &Db, journal_id: i64) -> Result<JournalEntry> {
    let entry = db.journal_entry(journal_id)?.with_context(|| {
        pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
    })?;
    if !undo_offered(&entry) {
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
    entry.dst.as_ref().context(pc_core::tr!(
        "в записи нет пути назначения",
        "the entry has no destination path"
    ))?;
    if let Err(e) = list_of(&entry) {
        let why = format!("{e:#}");
        return Err(told(db, journal_id, "undo", "refused", &why, e));
    }
    Ok(entry)
}

/// The interrupted entry `journal_id`, if its list can be read; one that
/// cannot is refused and written down.
pub(crate) fn reconcilable(db: &Db, journal_id: i64) -> Result<JournalEntry> {
    let entry = db.journal_entry(journal_id)?.with_context(|| {
        pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
    })?;
    if let Err(e) = readable(&entry) {
        let why = format!("{e:#}");
        return Err(told(db, journal_id, "reconcile", "refused", &why, e));
    }
    Ok(entry)
}

/// Record an undo whose files are back: the index follows, and the entry
/// is closed `undone` with what came back and where.
pub(crate) fn finish(
    db: &Db,
    entry: &JournalEntry,
    pairs: &[Pair],
    arrived: Vec<crate::bound::Arrived>,
    came: Vec<Moved>,
    decided: Option<&pc_db::ConflictNote>,
) -> Result<Tally> {
    let journal_id = entry.id;
    let dst = entry.dst.clone().unwrap_or_default();
    let mut done = Tally {
        files_back: came.len() as u64,
        ..Default::default()
    };
    // Where the entry's own file is now when it came back under a free name.
    let renamed = pairs
        .iter()
        .find(|p| p.rec.src == entry.src && p.renamed())
        .map(|p| p.look.src.clone());

    let unrecorded = |e: anyhow::Error, done: &Tally| -> anyhow::Error {
        let e = e.context(pc_core::tf!(
            "файлы вернулись, но журнал не записал откат записи {0}; повторный откат завершит его",
            "the files came back, but the journal did not record the undo of entry {0}; asking again finishes it",
            journal_id
        ));
        stop_run(e, done, Route::Restore, Vec::new())
    };
    let indexed = (|| -> Result<()> {
        // The index row follows a file that came back under a free name,
        // unless the index already holds a row at that name.
        let follow = |id: i64, at: &str| -> Result<()> {
            if db.file_id_at(at)?.is_none() {
                db.set_file_path(id, at, &name_of(Path::new(at)))?;
            }
            Ok(())
        };
        match entry.op.as_str() {
            "quarantine" => {
                if let Some(id) = db.bundle_id_at(&entry.src)? {
                    db.set_bundle_state(id, BundleState::Present)?;
                }
            }
            "quarantine-file" => {
                if let Some(id) = db.file_id_at(&entry.src)? {
                    if let Some(at) = &renamed {
                        follow(id, at)?;
                    }
                    db.set_file_state(id, "present")?;
                }
            }
            "organize" => {
                if let Some(id) = db.file_id_at(&dst)? {
                    follow(id, renamed.as_deref().unwrap_or(&entry.src))?;
                }
            }
            _ => {}
        }
        Ok(())
    })();
    if let Err(e) = indexed {
        return Err(unrecorded(e, &done));
    }
    // The decision on a taken place that brought it back, with its outcome,
    // written down before the entry closes (el-14vx0).
    let phase = if entry.status == JournalStatus::Pending {
        "reconcile"
    } else {
        "undo"
    };
    if let Some(note) = decided {
        let kind = note.outcome.as_deref().unwrap_or("done");
        let text = pc_core::tf!(
            "выбор «{0}» выполнен",
            "the choice “{0}” was carried out",
            note.choice
        );
        if let Err(e) = db.journal_event_conflict(journal_id, phase, kind, &text, note) {
            return Err(unrecorded(e, &done));
        }
    }
    // An interrupted entry brought back through a conflict choice is a
    // reconciliation, and its history says so.
    let (phase, text) = if entry.status == JournalStatus::Pending {
        (
            "reconcile",
            pc_core::tr!(
                "прерванная операция сверена по манифесту и отменена",
                "an interrupted operation was reconciled against its manifest and undone"
            ),
        )
    } else {
        ("undo", pc_core::tr!("откат выполнен", "undone"))
    };
    let closed = db.journal_close(
        journal_id,
        JournalStatus::Undone,
        &pc_db::Event {
            text,
            moved: &came,
            ..pc_db::Event::new(phase, "done")
        },
    );
    drop(arrived);
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
    Ok(pairs_of(&entry, pending_list(&entry)?)
        .iter()
        .map(item_of)
        .collect())
}

/// The list an interrupted entry is reconciled from: the same as an undo's
/// ([`list_of`]). A row written before the journal held a list includes
/// what carries the photograph's name beside it in quarantine — exactly
/// what the older versions moved with it — so that the photograph never
/// comes back without its sidecar and the entry is never closed over one
/// left behind (el-14vx0, R2-B1): the unit comes back whole, or nothing of
/// it does.
pub(crate) fn pending_list(entry: &JournalEntry) -> Result<Vec<Moved>> {
    list_of(entry)
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
/// `pending`, which is what it is, with the reason added to its history;
/// where the only obstacle is that its places are taken, the same choice as
/// an undo's is the person's ([`crate::reconcile_reviewed`]).
pub fn reconcile_undo(db: &Db, journal_id: i64) -> Result<Reconciled> {
    crate::conflict::reconcile_now(db, journal_id)
}

/// Close an interrupted entry whose files came back: the index follows,
/// and the entry is `undone`.
pub(crate) fn reconcile_close(
    db: &Db,
    entry: &JournalEntry,
    arrived: Vec<crate::bound::Arrived>,
    returned: Vec<Moved>,
) -> Result<Tally> {
    let journal_id = entry.id;
    let mut done = Tally {
        files_back: returned.len() as u64,
        ..Default::default()
    };
    let recorded = (|| -> Result<()> {
        if entry.op == "quarantine-file" {
            if let Some(id) = db.file_id_at(&entry.src)? {
                db.set_file_state(id, "present")?;
            }
        } else if entry.op == "quarantine" {
            if let Some(id) = db.bundle_id_at(&entry.src)? {
                db.set_bundle_state(id, BundleState::Present)?;
            }
        }
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
    drop(arrived);
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
    Ok(done)
}
