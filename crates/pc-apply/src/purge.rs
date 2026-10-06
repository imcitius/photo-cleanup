//! Permanent deletion of what a quarantine entry moved — of exactly that,
//! proven, and nothing else (el-3s9kp).
//!
//! `purge` is the only operation of this tool that deletes. It used to
//! delete the paths the journal recorded, recursively, without asking what
//! was there now: a stranger's file at the recorded name, a folder put in a
//! file's place with everything inside it, a quarantine folder replaced by a
//! link to somewhere else — each went, for good (el-lvtmk D6/S2).
//!
//! Now an entry is deleted only on proof, and only through held folders:
//!
//! 1. *Reach.* Every recorded object is reached from the first
//!    `.photo-cleanup-quarantine` component of its recorded path down,
//!    one folder at a time, `O_NOFOLLOW`: a link anywhere inside the
//!    quarantine is never followed (a link above it is the user's own path
//!    to their archive, as everywhere else in this crate, §11.2 (5а)).
//! 2. *Prove.* The object is opened through its held folder without
//!    following a link and compared, through the open descriptor, with the
//!    evidence the operation recorded before its first move — device, inode,
//!    kind, size, modification and birth time, the journaled hash where there
//!    is one ([`Proof::check_file`]); the name in the folder must still lead
//!    to that open object. Only a regular file is a file to delete.
//! 3. *All or none.* A photograph and its companions are one unit: one
//!    member that does not prove refuses the whole entry, before anything is
//!    deleted. The refusal is written to the entry's history with every
//!    member and why, and the entry stays in quarantine, undoable.
//! 4. *Delete.* Each file is compared again immediately before `unlinkat`
//!    through its held folder, and afterwards the held descriptor says what
//!    actually went: our object losing a link, or — if something replaced it
//!    in the instant between — that what was removed was not proven ours.
//!
//! A bundle of derived previews is one folder the tool moved whole. Its own
//! identity is proven like a file's (device, inode, birth time). Its
//! contents are proven against the list of every entry the journal wrote
//! when it moved ([`crate::contents`], el-8s63g B1): the tree is read first,
//! through held folders, and must hold exactly those entries, each the same
//! object, nothing that is not a regular file or a folder, nothing on
//! another device — and every file must be something the bundle's owner
//! rebuilds, by name and leading bytes. A photograph inside, however it got
//! there, keeps the bundle; so does a bundle moved before the list was kept.
//! Only then is it taken apart: each file compared with its evidence
//! immediately before its unlink, each folder removed only empty
//! (`AT_REMOVEDIR`, which the system refuses for a folder with anything in
//! it) and only when its name still leads to the folder that was read.
//! Nothing is removed recursively by path.
//!
//! What went is never lost on the way out: an error after the first unlink —
//! a stop, the journal refusing the end, the index not following — is a
//! [`PurgeStopped`] carrying what was really deleted (el-8s63g B2).
//!
//! The quarantine folders themselves (`.photo-cleanup-quarantine` and what a
//! gathered quarantine mirrors under it) are left in place.
//!
//! What this cannot close: POSIX has no conditional unlink. Between the last
//! comparison and `unlinkat` another program of the same user can put
//! something else under the name, and it goes instead. That deliberate
//! interleaving is outside the threat model (§11.2, el-3wizg); it is
//! detected afterwards and reported, not prevented.

use anyhow::{anyhow, bail, Result};
use pc_core::proof::{Kind, Proof};
use pc_db::{Db, JournalEntry, JournalStatus};

use crate::{recovery, Tally};

/// How far a purge that stopped after deleting had got (el-8s63g B2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeStage {
    /// Stopped while deleting: part of the entry may already be gone.
    Deleting,
    /// Everything was deleted; the journal did not take the end of it.
    Recording,
    /// Deleted and journaled; the index did not follow.
    Indexing,
}

/// A purge that stopped after it had deleted something: what really went,
/// and why it stopped. It is the one destructive outcome every caller
/// reports — the command line prints it and then fails, the web keeps it in
/// its job — so that no caller takes an error for "nothing was deleted".
/// Unless the stage is [`PurgeStage::Indexing`], the entry stays `pending`:
/// it is no longer an intact, undoable quarantine.
#[derive(Debug)]
pub struct PurgeStopped {
    pub entry: i64,
    pub done: Tally,
    pub reason: String,
    pub stage: PurgeStage,
}

impl std::fmt::Display for PurgeStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self.stage {
            PurgeStage::Deleting => pc_core::tf!(
                "окончательное удаление записи {0} остановлено: {1}. До остановки удалено: {2}; \
                 остальное не тронуто, запись остаётся незавершённой",
                "permanent deletion of entry {0} stopped: {1}. Deleted before the stop: {2}; \
                 the rest is untouched and the entry stays pending",
                self.entry,
                self.reason,
                self.done.summary()
            ),
            PurgeStage::Recording => pc_core::tf!(
                "запись {0}: всё перенесённое удалено ({2}), но журнал не принял завершение: {1}. \
                 Запись остаётся незавершённой; её файлов больше нет, откатывать нечего",
                "entry {0}: everything it moved was deleted ({2}), but the journal did not take \
                 the end of it: {1}. The entry stays pending; its files are gone and there is \
                 nothing to undo",
                self.entry,
                self.reason,
                self.done.summary()
            ),
            PurgeStage::Indexing => pc_core::tf!(
                "запись {0} удалена окончательно ({2}) и записана в журнал, но индекс не обновлён: {1}",
                "entry {0} was deleted for good ({2}) and journaled, but the index did not follow: {1}",
                self.entry,
                self.reason,
                self.done.summary()
            ),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for PurgeStopped {}

/// What a purge that failed had deleted all the same: nothing, unless it
/// stopped after deleting.
pub fn purged_before(e: &anyhow::Error) -> Tally {
    purge_stopped(e).map(|s| s.done.clone()).unwrap_or_default()
}

/// The stopped purge `e` carries, if it is one: something was deleted.
pub fn purge_stopped(e: &anyhow::Error) -> Option<&PurgeStopped> {
    e.downcast_ref::<PurgeStopped>()
}

/// Delete one quarantine entry for good, on proof (see the module).
///
/// `Ok` is the entry deleted whole, with what actually went. An error with
/// [`PurgeStopped`] in it deleted part of it; any other error deleted
/// nothing, and the entry is as it was.
/// Why purge keeps an entry rather than deleting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptWhy {
    /// Something of Lightroom's: a catalogue, its data, its previews, smart
    /// previews or helper indices, a backup of it.
    Lightroom,
    /// A bundle — a folder, or a file, moved whole by `derived clean`.
    Bundle,
}

/// An entry purge does not delete, whatever proof it has, and why — with
/// where it is in quarantine and how big it was when it moved, so a person
/// can delete it by hand (el-3s9kp, the user's decision of 2026-10-06:
/// "Lightroom catalogues are not to be touched at all").
///
/// Purge used to delete bundles too, after admitting their content by a
/// list and by the first bytes of each file. Three reviews in a row found
/// something irreplaceable that the admission let through (a photograph,
/// a changed file of the same size, a catalogue under a `.db` name,
/// el-8s63g, el-wffu8, el-5gr1y): a few bytes cannot prove a cache can be
/// rebuilt. So purge deletes no bundle at all, and nothing of Lightroom
/// even as a single file with matching evidence. The entry stays in
/// quarantine, undoable; its history records this outcome each time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeKept {
    pub entry: i64,
    pub why: KeptWhy,
    /// `(path in quarantine, bytes recorded when it moved)`.
    pub held: Vec<(String, u64)>,
    /// Files recorded when it moved.
    pub files: u64,
}

impl PurgeKept {
    /// The words for why, without the list.
    pub fn reason(&self) -> &'static str {
        match self.why {
            KeptWhy::Lightroom => pc_core::tr!(
                "это данные Lightroom (каталог, его данные, превью, смарт-превью, вспомогательные \
                 индексы или резервная копия), а их purge не удаляет никогда",
                "it is Lightroom's (a catalogue, its data, previews, smart previews, helper \
                 indices or a backup), and purge never deletes anything of Lightroom's"
            ),
            KeptWhy::Bundle => pc_core::tr!(
                "это бандл, перенесённый целиком, а purge не удаляет бандлов: что в нём, не \
                 доказывает, что это можно пересоздать",
                "it is a bundle moved whole, and purge deletes no bundle: nothing about what it \
                 holds proves that it can be rebuilt"
            ),
        }
    }

    /// `(path, why)` for the entry's history.
    pub fn listed(&self) -> Vec<(String, String)> {
        self.held
            .iter()
            .map(|(p, _)| (p.clone(), self.reason().to_string()))
            .collect()
    }
}

impl std::fmt::Display for PurgeKept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self
            .held
            .iter()
            .map(|(p, b)| format!("{p} ({})", pc_core::fmt_bytes(*b)))
            .collect::<Vec<_>>()
            .join("; ");
        let text = pc_core::tf!(
            "запись {0} оставлена — удалите вручную, если уверены: {1}. В карантине: {2}; \
             файлов при переносе: {3}. Ничего не удалено, запись можно откатить",
            "entry {0} is kept — delete it by hand if you are sure: {1}. In quarantine: {2}; \
             files when it moved: {3}. Nothing was deleted, and the entry can still be undone",
            self.entry,
            self.reason(),
            held,
            self.files
        );
        f.write_str(&text)
    }
}

impl std::error::Error for PurgeKept {}

/// The entry purge would keep, if any, from its record alone: what it
/// moved, not what is on disk now. A bundle is always kept; so is any
/// entry one of whose paths — where it came from or where it is — names
/// something of Lightroom's.
pub fn purge_keeps(e: &JournalEntry) -> Option<PurgeKept> {
    let why = if is_lightroom(&e.src)
        || e.dst.as_deref().is_some_and(is_lightroom)
        || e.manifest
            .iter()
            .any(|m| is_lightroom(&m.src) || is_lightroom(&m.dst))
    {
        KeptWhy::Lightroom
    } else if e.op == "quarantine" {
        KeptWhy::Bundle
    } else {
        return None;
    };
    let size = e.size.max(0) as u64;
    let held = match e.manifest.as_slice() {
        [] => vec![(e.dst.clone().unwrap_or_else(|| e.src.clone()), size)],
        [one] => vec![(one.dst.clone(), size)],
        many => many
            .iter()
            .map(|m| {
                let bytes = m.proof.as_ref().and_then(|p| p.size).unwrap_or(0);
                (m.dst.clone(), bytes)
            })
            .collect(),
    };
    Some(PurgeKept {
        entry: e.id,
        why,
        held,
        files: e.file_count.max(0) as u64,
    })
}

/// The kept outcome of `e`, if that is what `err` is.
pub fn purge_kept(err: &anyhow::Error) -> Option<&PurgeKept> {
    err.downcast_ref::<PurgeKept>()
}

/// Whether `path` names something of Lightroom's — by any of its parts, so
/// a file inside a `.lrdata` or `.lrcat-data` folder counts as well as the
/// folder. Lightroom's own names all end in an `.lr…` extension (`.lrcat`,
/// `.lrcat-data`, `.lrcat-wal`, `.lrdata`, `.lrprev`, `.lrlibrary`,
/// `.lrtemplate`…), and its backups are `.lrcat` copies or `.lrcat.zip`.
/// Anything else that happens to have such an extension is kept too: a
/// file kept by mistake costs one decision, a catalogue lost costs years.
pub fn is_lightroom(path: &str) -> bool {
    std::path::Path::new(path).components().any(|c| match c {
        std::path::Component::Normal(n) => n
            .to_string_lossy()
            .to_lowercase()
            .split('.')
            .skip(1)
            .any(|ext| ext.starts_with("lr")),
        _ => false,
    })
}

pub fn purge_entry_controlled(db: &Db, id: i64, control: &pc_core::work::Control) -> Result<Tally> {
    let e = db.journal_entry(id)?.ok_or_else(|| {
        anyhow!(pc_core::tr!(
            "нет записи карантина",
            "no such quarantine entry"
        ))
    })?;
    if e.status != JournalStatus::Done || !matches!(e.op.as_str(), "quarantine" | "quarantine-file")
    {
        bail!(
            "{}",
            pc_core::tf!(
                "запись {0} не находится в карантине",
                "entry {0} is not in quarantine",
                id
            )
        );
    }
    // A list this version cannot read says nothing about which files beside
    // the entry are its own (el-1y8uo B2). It is a refusal like any other,
    // and journaled like one; the record itself is left as it is (el-8s63g
    // B3).
    if let Some(kept) = purge_keeps(&e) {
        return Err(keep(db, &e, kept));
    }
    if let Err(unreadable) = recovery::readable(&e) {
        return Err(refuse(db, &e, &format!("{unreadable:#}"), &[]));
    }
    // An entry with an object found away from its record, or whose place
    // could not be proven, is not deleted at all: the record no longer says
    // where its objects are, and a deletion does not go looking (el-lvtmk
    // D6b, kept by el-3s9kp).
    if !e.located.is_empty() {
        let why = pc_core::tf!(
            "место перенесённого не подтверждено по записанным путям ({0})",
            "what it moved is not proven to be at its recorded paths ({0})",
            e.located
                .iter()
                .map(|l| format!("{} — {}", l.src, l.at.shown()))
                .collect::<Vec<_>>()
                .join("; ")
        );
        let listed: Vec<(String, String)> = e
            .located
            .iter()
            .map(|l| (l.dst.clone(), l.at.shown()))
            .collect();
        return Err(refuse(db, &e, &why, &listed));
    }
    control.check()?;
    run(db, &e, control)
}

/// Refuse the whole entry, nothing deleted: written into its history with
/// every member and why, the entry left `done` and undoable.
fn keep(db: &Db, e: &JournalEntry, kept: PurgeKept) -> anyhow::Error {
    let shown = kept.to_string();
    let recorded = db.journal_close(
        e.id,
        JournalStatus::Done,
        &pc_db::Event {
            text: &shown,
            refused: &kept.listed(),
            ..pc_db::Event::new("purge", "kept")
        },
    );
    let err = anyhow::Error::new(kept);
    match recorded {
        Ok(()) => err,
        Err(pe) => err.context(pc_core::tf!(
            "журнал не принял запись об этом ({0}); запись {1} не дополнена",
            "the journal did not take the record of this ({0}); entry {1} was not updated",
            format!("{pe:#}"),
            e.id
        )),
    }
}

fn refuse(db: &Db, e: &JournalEntry, why: &str, listed: &[(String, String)]) -> anyhow::Error {
    let shown = pc_core::tf!(
        "запись {0}: окончательное удаление отказано, ничего не удалено — {1}",
        "entry {0}: permanent deletion refused, nothing was deleted — {1}",
        e.id,
        why
    );
    let recorded = db.journal_close(
        e.id,
        JournalStatus::Done,
        &pc_db::Event {
            text: &shown,
            refused: listed,
            error: Some(&shown),
            ..pc_db::Event::new("purge", "refused")
        },
    );
    match recorded {
        Ok(()) => anyhow!("{shown}"),
        Err(pe) => anyhow!("{shown}").context(pc_core::tf!(
            "журнал не принял запись об этом ({0}); запись {1} не дополнена",
            "the journal did not take the record of this ({0}); entry {1} was not updated",
            format!("{pe:#}"),
            e.id
        )),
    }
}

#[cfg(not(unix))]
fn run(db: &Db, e: &JournalEntry, _: &pc_core::work::Control) -> Result<Tally> {
    // Deleting by proof needs held folders, links that are not followed and
    // object identity, none of which is verified on a real Windows system
    // (el-usdqi D4): nothing is deleted there.
    Err(refuse(
        db,
        e,
        pc_core::tr!(
            "на Windows окончательное удаление не выполняется: личность файлов и удаление через \
             удерживаемую папку на настоящей системе Windows не проверены",
            "on Windows permanent deletion is not carried out: file identity and deletion \
             through a held folder have not been verified on a real Windows system"
        ),
        &[],
    ))
}

#[cfg(unix)]
fn run(db: &Db, e: &JournalEntry, control: &pc_core::work::Control) -> Result<Tally> {
    use unix::{Member, Removed};

    // `purge_keeps` has already kept every bundle; this is the second lock
    // on that door, so a future caller of `run` cannot open it.
    if e.op != "quarantine-file" {
        return Err(keep(
            db,
            e,
            PurgeKept {
                entry: e.id,
                why: KeptWhy::Bundle,
                held: vec![(
                    e.dst.clone().unwrap_or_else(|| e.src.clone()),
                    e.size.max(0) as u64,
                )],
                files: e.file_count.max(0) as u64,
            },
        ));
    }

    if e.manifest.is_empty() {
        // A row from before lists were kept: what beside it is its own is
        // only a guess by name, and there is no evidence to prove anything
        // against. Such a quarantine is left for a person to delete.
        return Err(refuse(
            db,
            e,
            pc_core::tr!(
                "запись сделана до того, как журнал хранил список перенесённого и доказательство; \
                 доказать, что лежит по её путям, нечем — удалите вручную, проверив",
                "the entry predates the journal's list of what moved and its evidence; there is \
                 nothing to prove what is at its paths against — delete it by hand after checking"
            ),
            &[],
        ));
    }

    // Prove every member before deleting any.
    let mut members = Vec::new();
    let mut refused = Vec::new();
    for m in &e.manifest {
        match Member::prove(m) {
            Ok(member) => members.push(member),
            Err(why) => refused.push((m.dst.clone(), why)),
        }
    }
    if !refused.is_empty() {
        let why = refused
            .iter()
            .map(|(p, w)| format!("{p}: {w}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(refuse(db, e, &why, &refused));
    }

    // Proving may have taken a while (a hash): a stop asked for meanwhile
    // is honoured here, with nothing deleted and the entry as it was. Past
    // this point a photograph and its companions go together.
    control.check()?;
    db.journal_close(
        e.id,
        JournalStatus::Pending,
        &pc_db::Event {
            text: "Окончательное удаление начато; каждый объект доказан, при прерывании часть файлов уже может отсутствовать",
            ..pc_db::Event::new("purge", "begun")
        },
    )?;

    // The photograph last: if anything stops part way, what is left is the
    // thing that matters, not its sidecar.
    let frame = e.src.as_str();
    members.sort_by_key(|m| m.src() == frame);
    let mut removed = Removed::default();
    for m in &members {
        if let Err(why) = m.delete(control, &mut removed) {
            return Err(stopped(db, e, &removed, why));
        }
    }
    if !removed.strangers.is_empty() {
        let why = pc_core::tf!(
            "после последней проверки под именем оказался другой объект, и удалён был он: {0}; \
             проверенный объект остался на месте",
            "after the last check another object took the name, and it is what was removed: {0}; \
             the checked object is still there",
            removed.strangers.join("; ")
        );
        return Err(stopped(db, e, &removed, why));
    }

    let done = Tally {
        purged_entries: 1,
        ..removed.tally()
    };
    let text = pc_core::tf!(
        "Удалено окончательно: {0}",
        "Deleted for good: {0}",
        done.summary()
    );
    // From here on everything is gone. An error is no longer "nothing was
    // deleted": it carries what went, and is written down where it can be
    // (el-8s63g B2).
    if let Err(err) = db.journal_close(
        e.id,
        JournalStatus::Purged,
        &pc_db::Event {
            text: &text,
            ..pc_db::Event::new("purge", "done")
        },
    ) {
        return Err(after_deletion(
            db,
            e,
            done,
            PurgeStage::Recording,
            format!("{err:#}"),
        ));
    }
    // By path, for the same reason as in `undo`: the number in the entry may
    // now belong to a file that is still in the archive, and marking that one
    // purged would take it out of every view while its bytes sit untouched.
    // Only a file entry gets here: a bundle is kept ([`purge_keeps`]).
    let indexed = db
        .file_id_at(&e.src)
        .and_then(|id| id.map_or(Ok(()), |id| db.set_file_state(id, "purged")));
    if let Err(err) = indexed {
        return Err(after_deletion(
            db,
            e,
            done,
            PurgeStage::Indexing,
            format!("{err:#}"),
        ));
    }
    Ok(done)
}

/// Everything the entry moved was deleted, and a write after that failed:
/// the typed outcome with what really went, and an event saying so — the
/// entry `pending` if its end was not recorded, `purged` if it was.
#[cfg(unix)]
fn after_deletion(
    db: &Db,
    e: &JournalEntry,
    done: Tally,
    stage: PurgeStage,
    reason: String,
) -> anyhow::Error {
    let stop = PurgeStopped {
        entry: e.id,
        done,
        reason,
        stage,
    };
    let shown = stop.to_string();
    let status = match stage {
        PurgeStage::Indexing => JournalStatus::Purged,
        PurgeStage::Deleting | PurgeStage::Recording => JournalStatus::Pending,
    };
    let recorded = db.journal_close(
        e.id,
        status,
        &pc_db::Event {
            text: &shown,
            error: Some(&shown),
            ..pc_db::Event::new("purge", "unfinished")
        },
    );
    let err = anyhow::Error::new(stop);
    match recorded {
        Ok(()) => err,
        Err(pe) => err.context(pc_core::tf!(
            "журнал не принял запись об этом ({0}); запись {1} не дополнена",
            "the journal did not take the record of this ({0}); entry {1} was not updated",
            format!("{pe:#}"),
            e.id
        )),
    }
}

/// Part of the entry went; the entry stays `pending` with what went, what
/// did not, and why.
#[cfg(unix)]
fn stopped(db: &Db, e: &JournalEntry, removed: &unix::Removed, why: String) -> anyhow::Error {
    let done = removed.tally();
    let stop = PurgeStopped {
        entry: e.id,
        done,
        reason: why,
        stage: PurgeStage::Deleting,
    };
    let shown = stop.to_string();
    let mut listed: Vec<(String, String)> = removed
        .gone
        .iter()
        .map(|p| (p.clone(), pc_core::tr!("удалён", "deleted").to_string()))
        .collect();
    listed.extend(removed.strangers.iter().map(|p| {
        (
            p.clone(),
            pc_core::tr!(
                "удалён объект, не доказанный своим",
                "an object not proven ours was removed"
            )
            .to_string(),
        )
    }));
    let recorded = db.journal_close(
        e.id,
        JournalStatus::Pending,
        &pc_db::Event {
            text: &shown,
            refused: &listed,
            error: Some(&shown),
            ..pc_db::Event::new("purge", "partial")
        },
    );
    let err = anyhow::Error::new(stop);
    match recorded {
        Ok(()) => err,
        Err(pe) => err.context(pc_core::tf!(
            "журнал не принял запись об этом ({0}); запись {1} не дополнена",
            "the journal did not take the record of this ({0}); entry {1} was not updated",
            format!("{pe:#}"),
            e.id
        )),
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use pc_core::anchored::{ident_of, Dir, Ident};
    use pc_core::proof::Verdict;
    use pc_db::Moved;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path};

    const S_IFMT: u32 = 0o170_000;
    const S_IFREG: u32 = 0o100_000;
    const S_IFDIR: u32 = 0o040_000;

    /// What went, as it went.
    #[derive(Default)]
    pub(super) struct Removed {
        pub(super) files: u64,
        pub(super) bytes: u64,
        /// Paths deleted, for the history of a purge that stops.
        pub(super) gone: Vec<String>,
        /// Names under which something other than the checked object was
        /// removed (the residual window, detected after the fact).
        pub(super) strangers: Vec<String>,
    }

    impl Removed {
        pub(super) fn tally(&self) -> Tally {
            Tally {
                purged_files: self.files,
                purged_bytes: self.bytes,
                ..Default::default()
            }
        }
    }

    /// One file an entry moved, reached, proven and held open.
    pub(super) struct Member {
        moved: Moved,
        folder: Dir,
        name: String,
        file: fs::File,
        proof: Proof,
    }

    fn name_str(n: &std::ffi::OsStr) -> Result<String, String> {
        n.to_str().map(str::to_string).ok_or_else(|| {
            pc_core::tf!(
                "имя {0} не в UTF-8",
                "the name {0} is not UTF-8",
                n.to_string_lossy()
            )
        })
    }

    /// The folder that holds `path`, reached without following a link from
    /// the first quarantine folder of the path down, and the entry's name.
    fn reach(path: &Path) -> Result<(Dir, String), String> {
        let parts: Vec<Component> = path.components().collect();
        let at = parts
            .iter()
            .position(|c| matches!(c, Component::Normal(n) if *n == pc_core::QUARANTINE_DIR))
            .ok_or_else(|| {
                pc_core::tf!(
                    "записанный путь не лежит в папке карантина {0}",
                    "the recorded path does not lie in a quarantine folder {0}",
                    pc_core::QUARANTINE_DIR
                )
            })?;
        if parts.len() < at + 2 {
            return Err(pc_core::tr!(
                "записанный путь — сама папка карантина",
                "the recorded path is the quarantine folder itself"
            )
            .into());
        }
        let base: std::path::PathBuf = parts[..at].iter().collect();
        let base = if base.as_os_str().is_empty() {
            std::path::PathBuf::from(".")
        } else {
            base
        };
        let open_err = |p: &Path, e: std::io::Error| {
            pc_core::tf!(
                "папку {0} не открыть, не проходя по ссылке: {1}",
                "the folder {0} cannot be opened without following a link: {1}",
                p.display(),
                e
            )
        };
        let mut dir = Dir::open_following(&base).map_err(|e| open_err(&base, e))?;
        let last = parts.len() - 1;
        for c in &parts[at..last] {
            let Component::Normal(n) = c else {
                return Err(pc_core::tf!(
                    "в записанном пути есть «{0}»",
                    "the recorded path contains “{0}”",
                    c.as_os_str().to_string_lossy()
                ));
            };
            let n = name_str(n)?;
            dir = dir.open_dir(&n).map_err(|e| open_err(&dir.join(&n), e))?;
        }
        let Component::Normal(name) = parts[last] else {
            return Err(pc_core::tr!(
                "у записанного пути нет имени",
                "the recorded path has no name"
            )
            .into());
        };
        Ok((dir, name_str(name)?))
    }

    fn mode_at(dir: &Dir, name: &str) -> Result<(Ident, u32), String> {
        dir.stat_at(name).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                pc_core::tr!(
                    "его нет на записанном месте",
                    "it is not at its recorded place"
                )
                .to_string()
            } else {
                pc_core::tf!("не прочитать: {0}", "cannot read it: {0}", e)
            }
        })
    }

    fn verdict(v: Verdict) -> Result<(), String> {
        match v {
            Verdict::Same => Ok(()),
            Verdict::Differs(w) | Verdict::Unprovable(w) => Err(pc_core::tf!(
                "это не тот объект, что перенесла операция ({0})",
                "it is not the object the operation moved ({0})",
                w
            )),
        }
    }

    impl Member {
        pub(super) fn src(&self) -> &str {
            &self.moved.src
        }

        /// Reach and prove one recorded object; the answer is why not. Only
        /// a regular file, reached without following a link, that is what
        /// the entry moved by every field of its recorded evidence
        /// ([`Proof::check_file`], the content hash re-read where one is
        /// recorded). A folder is never deleted: purge deletes no bundle,
        /// and a folder at a file's place is not what moved (el-3s9kp).
        pub(super) fn prove(m: &Moved) -> Result<Member, String> {
            let proof = m.proof.as_ref().ok_or_else(|| {
                pc_core::tr!(
                    "журнал не записал доказательства, каким был этот объект",
                    "the journal recorded no evidence of what this object was"
                )
                .to_string()
            })?;
            if proof.kind != Kind::File {
                return Err(pc_core::tr!(
                    "журнал записал папку, а папок purge не удаляет",
                    "the journal recorded a folder, and purge deletes no folder"
                )
                .into());
            }
            let (folder, name) = reach(Path::new(&m.dst))?;
            let (ident, mode) = mode_at(&folder, &name)?;
            match mode & S_IFMT {
                S_IFREG => {}
                S_IFDIR => {
                    return Err(pc_core::tr!(
                        "на месте файла лежит папка",
                        "a folder is where a file belongs"
                    )
                    .into())
                }
                _ => {
                    return Err(pc_core::tr!(
                        "на записанном месте не обычный файл (ссылка, устройство, канал); \
                         по ссылке не переходим",
                        "at the recorded place is not a regular file (a link, a device, a \
                         pipe); links are not followed"
                    )
                    .into())
                }
            }
            let file = folder.open_file(&name, false).map_err(|e| {
                pc_core::tf!(
                    "не открыть, не проходя по ссылке: {0}",
                    "cannot be opened without following a link: {0}",
                    e
                )
            })?;
            verdict(proof.check_file(&file))?;
            let held = file.metadata().map_err(|e| e.to_string())?;
            if ident_of(&held) != ident || !held.is_file() {
                return Err(pc_core::tr!(
                    "имя перестало вести к открытому объекту",
                    "the name stopped leading to the opened object"
                )
                .into());
            }
            Ok(Member {
                moved: m.clone(),
                folder,
                name,
                file,
                proof: proof.clone(),
            })
        }

        /// Delete what was proven; the answer is why the deletion stopped.
        pub(super) fn delete(
            &self,
            control: &pc_core::work::Control,
            out: &mut Removed,
        ) -> Result<(), String> {
            // Not a cancellation point: a photograph and its companions go
            // together once the first of them has gone.
            control.progress.lock().unwrap().current = self.moved.dst.clone();
            unlink_proven(
                &self.folder,
                &self.name,
                &self.file,
                &self.proof,
                &self.moved.dst,
                out,
            )
        }
    }

    /// The last comparison, `unlinkat` through the held folder, and what the
    /// held descriptor says went.
    fn unlink_proven(
        dir: &Dir,
        name: &str,
        file: &fs::File,
        proof: &Proof,
        shown: &str,
        out: &mut Removed,
    ) -> Result<(), String> {
        // The same full comparison as when it was proven, through the held
        // descriptor: every recorded field, the content hash re-read whole
        // where one was recorded.
        verdict(proof.check_file(file)).map_err(|w| format!("{shown}: {w}"))?;
        let before = file.metadata().map_err(|e| format!("{shown}: {e}"))?;
        let (now, mode) = mode_at(dir, name).map_err(|w| format!("{shown}: {w}"))?;
        if now != ident_of(&before) || mode & S_IFMT != S_IFREG {
            return Err(pc_core::tf!(
                "{0} — имя перестало вести к проверенному файлу",
                "{0} — the name stopped leading to the checked file",
                shown
            ));
        }
        #[cfg(test)]
        seam::fire(dir, name);
        dir.remove_file_at(name)
            .map_err(|e| pc_core::tf!("не удалить {0}: {1}", "cannot delete {0}: {1}", shown, e))?;
        let after = file.metadata().map_err(|e| e.to_string())?;
        if after.nlink() + 1 == before.nlink() {
            out.files += 1;
            // Space comes back only with the last link.
            if after.nlink() == 0 {
                out.bytes += after.len();
            }
            out.gone.push(shown.to_string());
        } else {
            out.strangers.push(shown.to_string());
        }
        Ok(())
    }

    /// Tests only: the instant between the last comparison and `unlinkat`,
    /// which nothing can close (POSIX has no conditional unlink).
    #[cfg(test)]
    pub(crate) mod seam {
        use pc_core::anchored::Dir;
        use std::cell::RefCell;

        type Hook = Box<dyn FnMut(&std::path::Path)>;
        thread_local! {
            static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
        }

        pub(crate) struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                HOOK.with(|h| *h.borrow_mut() = None);
            }
        }

        /// While the guard lives, `hook(path)` runs right before each unlink.
        pub(crate) fn before_unlink(hook: impl FnMut(&std::path::Path) + 'static) -> Guard {
            HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
            Guard
        }

        pub(super) fn fire(dir: &Dir, name: &str) {
            HOOK.with(|h| {
                if let Some(hook) = h.borrow_mut().as_mut() {
                    hook(&dir.join(name))
                }
            })
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) use unix::seam;
