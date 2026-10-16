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

use crate::located::Way;
use crate::unit::{move_unit, Member, Unit};
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
enum Spot {
    Absent,
    Proven,
    /// Something is there; the entry recorded nothing to compare it with.
    Unproven,
    /// Something is there and it is not the file, or cannot be shown to be.
    Other(&'static str),
    Unreadable(String),
}

#[cfg(not(windows))]
fn spot(path: &str, proof: Option<&Proof>) -> Spot {
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

/// Windows (el-61ar8 §2.5): a path's metadata names no object there, so the
/// entry is read as itself through its folder's handle — the folder
/// followed as the user named it, the last component not — and compared by
/// [`Proof::verify`]. A missing folder or name is nothing there; any other
/// failure says nothing about what is there.
#[cfg(windows)]
fn spot(path: &str, proof: Option<&Proof>) -> Spot {
    let at = Path::new(path);
    let (Some(parent), Some(name)) = (
        at.parent().filter(|p| !p.as_os_str().is_empty()),
        at.file_name().and_then(|n| n.to_str()),
    ) else {
        return Spot::Unreadable(pc_core::tf!(
            "не путь к файлу: {0}",
            "not a path to a file: {0}",
            path
        ));
    };
    let dir = match pc_core::anchored::Dir::open_following(parent) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Spot::Absent,
        Err(e) => return Spot::Unreadable(e.to_string()),
    };
    match dir.entry_at(name) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Spot::Absent,
        Err(e) => return Spot::Unreadable(e.to_string()),
        Ok(_) => {}
    }
    let Some(p) = proof else {
        return Spot::Unproven;
    };
    match pc_core::proof::Proof::at(&dir, name) {
        Some(now) => match p.verify(&now, None) {
            Verdict::Same => Spot::Proven,
            Verdict::Differs(w) | Verdict::Unprovable(w) => Spot::Other(w),
        },
        None => Spot::Other("this system cannot name a file system object"),
    }
}

/// The evidence against what is at `path`, by [`Proof::verify`]: through
/// an open descriptor when the evidence records a hash, so the content is
/// compared too; a hash that cannot be read again is not a match.
#[cfg(not(windows))]
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
    // Windows (el-usdqi D4, el-6s3go): a row without evidence is never
    // walked back there by its names. Every row this tool writes on Windows
    // records evidence; one without it came from elsewhere, and a file at
    // its recorded quarantine path is not the operation's on its name alone.
    #[cfg(windows)]
    if m.proof.is_none() {
        return Standing::Doubt(pc_core::tf!(
            "{0} — запись не хранит сведений о том, какой файл перенесён, а на Windows файл по одному имени не возвращается; он остаётся на месте: {1}",
            "{0} — the entry records no evidence of which file moved, and on Windows a file is not brought back on its name alone; it stays where it is: {1}",
            m.src,
            m.dst
        ));
    }
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

/// One item of an entry: as recorded, and where recovery looks for it.
///
/// The record is never rewritten. Where the operation later found the
/// object away from its recorded place — its folder moved under it, a
/// return that could not finish — a `located` event says where it was
/// proven or last seen (el-lvtmk R5), and recovery looks there instead of at
/// the recorded `dst`. Whatever is found there is still this entry's only on
/// its evidence ([`classify`]); the overlay is a place to look, never a
/// reason to act.
#[derive(Debug, Clone)]
struct Pair {
    rec: Moved,
    look: Moved,
    note: Option<String>,
    /// The object the operation last found is its own, changed since its
    /// check, and was kept where this says (user decision (c), point 4).
    changed: Option<String>,
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

fn item_of(p: &Pair) -> Item {
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
    Ok(list_of(entry)?
        .into_iter()
        .map(|m| item_of(&pair_of(entry, m)))
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
fn told(db: &Db, id: i64, phase: &str, kind: &str, text: &str, e: anyhow::Error) -> anyhow::Error {
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
fn listing(items: &[Item]) -> String {
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
fn walk_back(
    db: &Db,
    id: i64,
    op: &str,
    phase: &str,
    pairs: &[Pair],
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
    let moving: Vec<&Pair> = pairs
        .iter()
        .zip(&items)
        .filter(|(_, i)| i.standing == Standing::Moved)
        .map(|(p, _)| p)
        .collect();
    if moving.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    // A photograph and its sidecars come back as plain files only, the rule
    // they left by ([`files::not_a_plain_file`]): evidence that names a link,
    // a folder or another object in a file entry is not walked back, and
    // nothing of the unit moves. A gathered folder of previews (`quarantine`)
    // is a folder by its nature. A row from before evidence was recorded is
    // bound to what it finds and, through it, to the same arrival check.
    if op != "quarantine" {
        if let Some(p) = moving
            .iter()
            .find(|p| p.look.proof.is_some() && !files::plain_proof(p.look.proof.as_ref()))
        {
            let why = pc_core::tf!(
                "{0}: ничего не перенесено — {1} записан не как обычный файл, а возвращаются только обычные файлы; всё остаётся на месте: {2}",
                "{0}: nothing was moved — {1} was recorded as something other than a plain file, and only plain files come back; everything stays where it is: {2}",
                phase,
                p.look.dst,
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
    }
    // The folder each member comes back to has to be the folder it left.
    // Recorded at the move, its identity decides (director decision after
    // el-4r8yf B3-R2): the folder is opened once, compared with the record
    // before any rename, and that handle is where the rename goes. A row
    // from before identities were recorded is judged by its path, checked
    // up to its own run's root and no other (el-1nx1u B3). A row whose run
    // recorded no roots at all (older web runs) is bounded by the volume
    // itself and comes back into a folder held from its mount point down
    // (el-53l7h, `crate::mount_bound`); folders it needs are made only after
    // every member's way is checked. Refused, nothing moves, every member
    // stays where it is proven, and the entry stays open to be undone again.
    let mut homes: Vec<Option<pc_core::anchored::Dir>> = Vec::with_capacity(moving.len());
    let mut own_roots: Option<Option<Vec<String>>> = None;
    let mut unsafe_at = None;
    #[cfg(unix)]
    let mut chains: Vec<(usize, crate::mount_bound::Chain)> = Vec::new();
    for (i, p) in moving.iter().enumerate() {
        let src = Path::new(&p.look.src);
        match &p.rec.home {
            Some(home) => match crate::bound::origin_folder(src, home) {
                Ok(dir) => homes.push(Some(dir)),
                Err(e) => {
                    unsafe_at = Some(format!("{e:#}"));
                    break;
                }
            },
            None => {
                if own_roots.is_none() {
                    own_roots = Some(db.journal_run_roots(id)?);
                }
                let roots = own_roots.as_ref().and_then(|r| r.as_deref());
                #[cfg(unix)]
                if roots.is_some_and(no_roots) {
                    let checked = match (src.parent(), p.look.proof.as_ref()) {
                        (Some(folder), Some(proof)) => {
                            crate::mount_bound::check(folder, proof.dev)
                        }
                        _ => Err(pc_core::tf!(
                            "{0} — запись не хранит, с какого тома ушёл файл, и прогон не записал своих папок; программа ничего туда не возвращает",
                            "{0} — the entry does not record which volume the file left, and its run recorded no folders; the tool returns nothing there",
                            src.display()
                        )),
                    };
                    match checked {
                        Ok(chain) => {
                            chains.push((i, chain));
                            homes.push(None);
                            continue;
                        }
                        Err(why) => {
                            unsafe_at = Some(why);
                            break;
                        }
                    }
                }
                let _ = i;
                if let Some(why) = src.parent().and_then(|f| origin_doubt(roots, f)) {
                    unsafe_at = Some(why);
                    break;
                }
                homes.push(None);
            }
        }
    }
    #[cfg(unix)]
    if unsafe_at.is_none() {
        for (i, chain) in chains {
            match chain.hold() {
                Ok(dir) => homes[i] = Some(dir),
                Err(why) => {
                    unsafe_at = Some(why);
                    break;
                }
            }
        }
    }
    if let Some(unsafe_at) = unsafe_at {
        drop(homes);
        let why = pc_core::tf!(
            "{0}: ничего не перенесено — {1}; всё остаётся в карантине, и запись можно откатить, когда на этом месте снова будет та папка; где что: {2}",
            "{0}: nothing was moved — {1}; everything stays in quarantine, and the entry can be undone once that folder is in that place again; where things are: {2}",
            phase,
            unsafe_at,
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
    let members: Vec<Member> = moving
        .iter()
        .zip(&homes)
        .map(|(p, h)| Member::back(&p.rec, &p.look, h.as_ref()))
        .collect();
    let pb = match move_unit(&members, Way::Back, None, &[]) {
        Unit::Moved(arrived) => {
            return Ok((arrived, moving.iter().map(|p| p.rec.clone()).collect()))
        }
        Unit::Refused(e) => {
            let why = pc_core::tf!(
                "{0}: ничего не перенесено — {1}; где что: {2}",
                "{0}: nothing was moved — {1}; where things are: {2}",
                phase,
                e,
                listing(&items)
            );
            let stop = crate::is_no_exclusive_rename(&e);
            let e = told(db, id, phase, "refused", &why, e);
            return Err(if stop { e } else { e.context(why) });
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
    Err(if stops {
        stop_run(e, &Tally::default(), Route::Restore, Vec::new())
    } else {
        e
    })
}

/// The run recorded no folder at all: an older web run (`roots = []`).
#[cfg(unix)]
fn no_roots(roots: &[String]) -> bool {
    roots.iter().all(|r| r.is_empty())
}

/// Why the folder `folder`, which a member of an entry that recorded no
/// folder identity comes back to, is not to be trusted, if it is not.
///
/// Only rows written before identities were recorded come here (on Unix;
/// Windows never had a quarantine before). What is asked is every component
/// from `folder` up to the root of the entry's *own* run that holds it
/// (`roots`; the deepest one), each as itself, never followed: a link or a
/// junction there — one that replaced the folder after the move — would
/// carry the photograph into someone else's folder while the journal
/// recorded it home (el-1nx1u B3). Roots of other runs are never asked: a
/// later scan of a folder deep inside would shorten the check
/// (el-4r8yf B3-R2). Above the root, the path is the user's own as they
/// named it (`/var` on macOS is a link) and is followed. A path under none
/// of its run's roots, or a run whose roots cannot be read, has no bound
/// and is refused. A run that recorded no roots at all (`roots = []`, older
/// web runs) does not come here on Unix: it is bounded by the volume's
/// mount point instead ([`crate::mount_bound`], el-53l7h). A folder that is not there is not a doubt: it is
/// created on the way back, through the folder above it, without following
/// anything.
fn origin_doubt(roots: Option<&[String]>, folder: &Path) -> Option<String> {
    let Some(root) = roots.and_then(|roots| {
        roots
            .iter()
            .map(Path::new)
            .filter(|r| !r.as_os_str().is_empty() && folder.starts_with(r))
            .max_by_key(|r| r.components().count())
    }) else {
        return Some(pc_core::tf!(
            "место возврата {0} не лежит ни под одной из папок, которые просматривал прогон этой записи, и запись не знает, какая папка была там при переносе; программа ничего туда не возвращает",
            "the place to return to, {0}, is under none of the folders this entry's run scanned, and the entry does not record which folder was there at the move; the tool returns nothing there",
            folder.display()
        ));
    };
    let mut at = Some(folder);
    while let Some(p) = at.filter(|p| !p.as_os_str().is_empty()) {
        let what = match files::found_at(p) {
            Ok(files::Found::Dir) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some(e.to_string()),
            Ok(files::Found::Link) => Some(
                pc_core::tr!(
                    "символическая ссылка или соединение (junction)",
                    "a symbolic link or a junction"
                )
                .to_string(),
            ),
            Ok(_) => Some(
                pc_core::tr!(
                    "не папка (файл, облачная заглушка или иной особый объект)",
                    "not a folder (a file, a cloud placeholder or another special object)"
                )
                .to_string(),
            ),
        };
        if let Some(what) = what {
            return Some(pc_core::tf!(
                "место возврата {0} — {1}; программа не переходит по нему и ничего туда не возвращает",
                "the place to return to, {0}, is {1}; the tool does not follow it and returns nothing there",
                p.display(),
                what
            ));
        }
        if p == root {
            break;
        }
        at = p.parent();
    }
    None
}

/// Walk a finished entry back: every file it moved, by its evidence, as one
/// unit.
pub fn undo(db: &Db, journal_id: i64) -> Result<Tally> {
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
    let pairs: Vec<Pair> = list.iter().map(|m| pair_of(&entry, m.clone())).collect();
    if pairs.iter().map(item_of).all(|i| i.why().is_none()) {
        adopt_held_evidence(db, journal_id, &mut list)?;
    }
    let pairs: Vec<Pair> = list.into_iter().map(|m| pair_of(&entry, m)).collect();
    let (arrived, came) = walk_back(db, journal_id, &entry.op, "undo", &pairs)?;
    let mut done = Tally {
        files_back: came.len() as u64,
        ..Default::default()
    };

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
        return Err(unrecorded(e, &done));
    }
    let closed = db.journal_close(
        journal_id,
        JournalStatus::Undone,
        &pc_db::Event {
            text: pc_core::tr!("откат выполнен", "undone"),
            moved: &came,
            ..pc_db::Event::new("undo", "done")
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
    Ok(pending_list(&entry)?
        .into_iter()
        .map(|m| item_of(&pair_of(&entry, m)))
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
/// retry recognises what is home by proof — never by its name. Rows that recorded evidence are left exactly as they are.
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
    if items.iter().any(|i| i.why().is_some()) {
        let why = pc_core::tf!(
            "сверка: ничего не перенесено — кадр со спутниками возвращается только целиком, а \
             не всё доказано: {0}",
            "reconcile: nothing was moved — the frame and its companions come back only \
             together, and not all of them are proven: {0}",
            listing(&items)
        );
        let e = anyhow::anyhow!("{why}");
        return Err(told(db, journal_id, "reconcile", "refused", &why, e));
    }
    // The evidence each move back is bound to (el-3wizg): as journaled, or
    // as just taken from the file at its recorded quarantine path.
    let (entry, pairs) = {
        let entry = db.journal_entry(journal_id)?.with_context(|| {
            pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
        })?;
        let mut list = pending_list(&entry)?;
        adopt_held_evidence(db, journal_id, &mut list)?;
        let pairs: Vec<Pair> = list.into_iter().map(|m| pair_of(&entry, m)).collect();
        (entry, pairs)
    };
    let (arrived, returned) = walk_back(db, journal_id, &entry.op, "reconcile", &pairs)?;
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
    Ok(Reconciled { items, done })
}
