//! An undo whose original place is taken: the user's choice (el-14vx0).
//!
//! Recovery brings a file back only to a free place (§11.2): another file at
//! its name — or one that cannot be proven to be it — keeps the entry open
//! and the file in quarantine. That stays the default. The user decided
//! (2026-10-03) that they may also choose, per conflict and for all the
//! remaining ones at once:
//!
//! 1. **keep** it in quarantine and return it by hand later — the refusal
//!    names its exact place in quarantine and where it belongs;
//! 2. **replace** the existing file: the *existing* unit is first carried
//!    into quarantine beside its folder, under an entry of its own that can
//!    be undone like any other, and then the returning unit goes home.
//!    Nothing is deleted (invariant 1);
//! 3. **rename the existing** file to a free name (`IMG_1.CR2`, `_2`, …)
//!    beside it, under an entry of its own, and then return the unit to its
//!    own name;
//! 4. **return the unit under a free name** and leave the existing file
//!    alone.
//!
//! What a choice moves is a whole unit (el-3wizg): the returning frame with
//! its companions, and the existing frame with its own `.xmp`/`.aae`/`._*`;
//! companions take their frame's new name. Any doubt about any member
//! refuses the whole unit, as everywhere else.
//!
//! A free name is never looked for and then taken: every move here is the
//! one bound, no-replace rename of [`crate::unit::move_unit`], and a name
//! that turns out taken at the rename ([`crate::Taken`]) — a file created
//! there between the choice and the move — moves nothing and the next name
//! is tried. A look before the rename only skips names already taken.
//!
//! The occupants are bound to the evidence read when the choice was offered
//! ([`Conflict`]): an occupant replaced or changed since — a foreign file
//! that appeared after the preview — is refused, never set aside in its
//! place. And a conflict that was not there when the choice was made gets
//! the default: it is kept, never replaced.
//!
//! Lightroom is never touched (user decision 2026-10-06, invariant 7): when
//! any file of the conflict is in a live catalogue or on a Lightroom path,
//! only "keep" and "return under a free name" are offered — the existing
//! file is neither renamed nor carried away. A bundle comes back only to its
//! own place.
//!
//! Every decision and its outcome are appended to the entry's history as a
//! structured event ([`pc_db::ConflictNote`]); a return under free names is
//! recorded *before* its rename, so a retry after an interruption finds what
//! already came back there — by its evidence ([`crate::recovery`]). A retry
//! after a stop between setting the occupants aside and the return finds the
//! place free and is an ordinary undo.

use anyhow::{anyhow, Context, Result};
use pc_core::proof::{Kind, Proof};
use pc_db::{ConflictNote, Db, JournalEntry, JournalStatus, Moved, ReturnedAs};
use std::fs;
use std::path::Path;

use crate::located::{Told, Way};
use crate::recovery::{self, Back, Item, Pair, Spot, Standing};
use crate::unit::{move_unit, Member, Unit};
use crate::{organize, stop_run, Route, Tally};

/// How far the free names go: `_1` to `_999`. Past that, the file stays
/// where it is and the refusal says so.
const MAX_SUFFIX: u32 = 999;

/// What to do with an undo whose original place is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Choice {
    /// Leave it in quarantine; the default.
    Keep,
    /// Carry the existing unit into quarantine, then return this one.
    Replace,
    /// Rename the existing unit to a free name, then return this one.
    RenameExisting,
    /// Return this unit under a free name; the existing one stays.
    RenameReturning,
}

impl Choice {
    pub const ALL: [Choice; 4] = [
        Choice::Keep,
        Choice::Replace,
        Choice::RenameExisting,
        Choice::RenameReturning,
    ];

    /// The word the command line takes (`--on-conflict`) and the web sends.
    pub fn as_str(self) -> &'static str {
        match self {
            Choice::Keep => "keep",
            Choice::Replace => "replace",
            Choice::RenameExisting => "rename-existing",
            Choice::RenameReturning => "rename-returning",
        }
    }

    pub fn parse(s: &str) -> Option<Choice> {
        Choice::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// In words, for a person.
    pub fn words(self) -> &'static str {
        match self {
            Choice::Keep => pc_core::tr!(
                "оставить в карантине, вернуть вручную потом",
                "keep it in quarantine, return it by hand later"
            ),
            Choice::Replace => pc_core::tr!(
                "заменить существующий: он уйдёт в карантин (не удаляется, откатывается)",
                "replace the existing file: it goes to quarantine (not deleted, can be undone)"
            ),
            Choice::RenameExisting => pc_core::tr!(
                "переименовать существующий в *_1, вернуть файл на его имя",
                "rename the existing file to *_1 and return this one to its name"
            ),
            Choice::RenameReturning => pc_core::tr!(
                "вернуть под именем *_1, существующий не трогать",
                "return it as *_1 and leave the existing file alone"
            ),
        }
    }
}

/// One member of the unit coming back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Returning {
    /// Its place, which something else bears (or, for a companion, may be
    /// free).
    pub home: String,
    /// Where it is held now.
    pub held: String,
    /// Its evidence, as the journal recorded it; `None` in rows written
    /// before evidence was kept.
    pub proof: Option<Proof>,
}

/// One file of the existing unit, as read when the choice was offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupant {
    pub path: String,
    pub proof: Proof,
}

impl Occupant {
    pub fn size(&self) -> u64 {
        self.proof.size.unwrap_or(0)
    }

    /// The object read, compactly: what a reviewed choice is bound to.
    pub fn evidence(&self) -> String {
        let p = &self.proof;
        format!(
            "{}:{}:{}:{}",
            p.dev,
            p.ino,
            p.size.unwrap_or(0),
            p.mtime_ns.unwrap_or(0)
        )
    }
}

/// An undo whose original place is taken, as read now — and the choices it
/// offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub journal_id: i64,
    /// The frame first, then its companions.
    pub returning: Vec<Returning>,
    /// What bears the places: the existing frame with its own companions,
    /// or a companion's place alone.
    pub occupants: Vec<Occupant>,
    /// Always starts with [`Choice::Keep`].
    pub choices: Vec<Choice>,
    /// Why a choice is not offered, in words.
    pub limits: Vec<String>,
}

impl Conflict {
    /// The conflict in words: what comes back, from where, and what bears
    /// its place.
    pub fn describe(&self) -> String {
        let back = self
            .returning
            .iter()
            .map(|r| format!("{} ← {}", r.home, r.held))
            .collect::<Vec<_>>()
            .join("; ");
        let there = self
            .occupants
            .iter()
            .map(|o| o.path.clone())
            .collect::<Vec<_>>()
            .join("; ");
        pc_core::tf!(
            "запись {0}: исходное место занято другим файлом. Возвращается: {1}. На месте лежит: {2}",
            "entry {0}: the original place is taken by another file. Coming back: {1}. In its place: {2}",
            self.journal_id,
            back,
            there
        )
    }

    /// The default's words: where the file stays and where it belongs.
    pub fn kept_words(&self) -> String {
        let r = &self.returning[0];
        pc_core::tf!(
            "{0}; оставлено в карантине: {1} — место, куда возвращать: {2}. Ничего не перенесено, \
             запись остаётся открытой; выберите «заменить» или «переименовать», чтобы вернуть \
             (`--on-conflict` или диалог на странице «Карантин»)",
            "{0}; kept in quarantine: {1} — it belongs at {2}. Nothing was moved and the entry \
             stays open; choose to replace or rename to bring it back (`--on-conflict`, or the \
             dialog on the Quarantine page)",
            self.describe(),
            r.held,
            r.home
        )
    }

    fn note(&self, choice: Choice) -> ConflictNote {
        ConflictNote {
            choice: choice.as_str().to_string(),
            returning: self
                .returning
                .iter()
                .map(|r| Moved {
                    src: r.home.clone(),
                    dst: r.held.clone(),
                    proof: r.proof.clone(),
                })
                .collect(),
            occupants: self
                .occupants
                .iter()
                .map(|o| Moved {
                    src: o.path.clone(),
                    dst: String::new(),
                    proof: Some(o.proof.clone()),
                })
                .collect(),
            returned_as: Vec::new(),
            aside_entry: None,
        }
    }
}

/// The file that keeps a conflict in quarantine, typed: the conflict and
/// the words. The run goes on to the next entry.
#[derive(Debug, Clone)]
pub struct ConflictKept {
    pub conflict: Conflict,
    pub why: String,
}

impl std::fmt::Display for ConflictKept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for ConflictKept {}

/// The kept conflict an undo's error carries, if that is why it refused.
pub fn conflict_kept(e: &anyhow::Error) -> Option<&ConflictKept> {
    e.chain().find_map(|c| c.downcast_ref::<ConflictKept>())
}

/// A choice made on a conflict seen earlier — in the web's preview — and
/// carried out later. It holds only while the conflict read then is the
/// conflict read now: the same files coming back, and the same objects, by
/// their evidence, bearing their places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewed {
    pub seen: Conflict,
    pub choice: Choice,
}

/// [`crate::undo`] with a choice reviewed beforehand. Without one, a
/// conflict found now is kept (it was not there to be reviewed); with one,
/// a conflict that is no longer the one reviewed is refused.
pub fn undo_reviewed(db: &Db, journal_id: i64, reviewed: Option<&Reviewed>) -> Result<Tally> {
    crate::undo_with(db, journal_id, &mut |now| match reviewed {
        None => Ok(Choice::Keep),
        Some(r) if r.seen == *now => Ok(r.choice),
        Some(_) => Err(anyhow!(pc_core::tf!(
            "запись {0}: с предпросмотра на исходном месте что-то изменилось; ничего не \
             перенесено — обновите предпросмотр и выберите снова. Сейчас: {1}",
            "entry {0}: something changed at the original place since the preview; nothing was \
             moved — refresh the preview and choose again. Now: {1}",
            now.journal_id,
            now.describe()
        ))),
    })
}

/// The conflict an undo of `entry` would meet now, if its only obstacle is
/// that its place is taken. Reads only.
pub fn undo_conflict(db: &Db, entry: &JournalEntry) -> Result<Option<Conflict>> {
    if !recovery::undo_offered(entry) {
        return Ok(None);
    }
    let Ok(list) = recovery::list_of(entry) else {
        return Ok(None);
    };
    of(db, entry, &recovery::pairs_of(entry, list))
}

fn stem(path: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    organize::stem_of(name).to_string()
}

/// `path` under the free name `n` of a unit whose frame is named `stem`:
/// `IMG.CR2` → `IMG_1.CR2`, `IMG.xmp` → `IMG_1.xmp`, `._IMG.CR2` →
/// `._IMG_1.CR2`. `None` when the name does not carry the stem, so that it
/// would not change.
fn suffixed(path: &str, stem: &str, n: u32) -> Option<String> {
    let p = Path::new(path);
    let name = p.file_name()?.to_str()?;
    let new = organize::sidecar_name(name, stem, &format!("{stem}_{n}"));
    (new != name).then(|| p.with_file_name(new).to_string_lossy().into_owned())
}

/// Lightroom's, by a live catalogue (full path or its tail, invariant 7) or
/// by a `.lr…` part of the path.
fn lightroom(db: &Db, paths: &[&str]) -> Result<bool> {
    if paths.iter().any(|p| crate::is_lightroom(p)) {
        return Ok(true);
    }
    let curated = pc_family::curation::CurationIndex::build(db.lightroom_protected()?);
    Ok(paths.iter().any(|p| curated.lookup(p).is_some()))
}

/// The conflict, when every item of the entry is held where it belongs —
/// proven by its evidence, or at its own recorded quarantine path in a row
/// written without evidence — and some of their places bear something that
/// is not proven to be the item. Anything else — a member gone, changed, a
/// stranger in quarantine, an unreadable place, a return under free names
/// already under way — is not a choice to offer: recovery refuses it as
/// before.
pub(crate) fn of(db: &Db, entry: &JournalEntry, pairs: &[Pair]) -> Result<Option<Conflict>> {
    let mut returning = Vec::new();
    let mut occupants: Vec<Occupant> = Vec::new();
    let frame = pairs
        .iter()
        .position(|p| p.rec.src == entry.src)
        .unwrap_or(0);
    for (i, p) in pairs.iter().enumerate() {
        if p.changed.is_some() || p.renamed() {
            return Ok(None);
        }
        let proof = p.look.proof.as_ref();
        match recovery::spot(&p.look.dst, proof) {
            Spot::Proven => {}
            Spot::Unproven if proof.is_none() => {}
            _ => return Ok(None),
        }
        match recovery::spot(&p.look.src, proof) {
            Spot::Absent => {}
            Spot::Other(_) | Spot::Unproven => {
                let Some(o) = occupant(&p.look.src) else {
                    return Ok(None);
                };
                occupants.push(o);
            }
            Spot::Proven | Spot::Unreadable(_) => return Ok(None),
        }
        let r = Returning {
            home: p.look.src.clone(),
            held: p.look.dst.clone(),
            proof: p.look.proof.clone(),
        };
        if i == frame {
            returning.insert(0, r);
        } else {
            returning.push(r);
        }
    }
    if occupants.is_empty() || returning.is_empty() {
        return Ok(None);
    }
    // The existing frame's own companions are its unit: they go with it
    // wherever it goes, and never stay behind as litter.
    let home = returning[0].home.clone();
    if occupants.iter().any(|o| o.path == home) {
        for side in crate::files::companions(Path::new(&home)) {
            let Some(o) = occupant(&side.to_string_lossy()) else {
                return Ok(None);
            };
            let ident = |x: &Occupant| (x.proof.dev, x.proof.ino);
            if !occupants.iter().any(|x| ident(x) == ident(&o)) {
                occupants.push(o);
            }
        }
    }

    let mut choices = vec![Choice::Keep];
    let mut limits = Vec::new();
    let file_op = matches!(
        entry.op.as_str(),
        "quarantine-file" | "organize" | "set-aside"
    );
    if !file_op {
        limits.push(
            pc_core::tr!(
                "папка возвращается только на своё место",
                "a folder comes back only to its own place"
            )
            .to_string(),
        );
    } else {
        let s = stem(&home);
        let mut paths: Vec<&str> = Vec::new();
        for r in &returning {
            paths.push(&r.home);
            paths.push(&r.held);
        }
        paths.extend(occupants.iter().map(|o| o.path.as_str()));
        if lightroom(db, &paths)? {
            limits.push(
                pc_core::tr!(
                    "Lightroom не трогается: существующий файл не заменяется и не \
                     переименовывается — только «оставить» или «вернуть под именем *_1»",
                    "Lightroom is never touched: the existing file is neither replaced nor \
                     renamed — only “keep” or “return as *_1”"
                )
                .to_string(),
            );
        } else if occupants.iter().any(|o| o.proof.kind != Kind::File) {
            limits.push(
                pc_core::tr!(
                    "на месте лежит не обычный файл (папка или ссылка): он не переносится",
                    "what is in its place is not a plain file (a folder or a link): it is not moved"
                )
                .to_string(),
            );
        } else {
            choices.push(Choice::Replace);
            if occupants.iter().all(|o| suffixed(&o.path, &s, 1).is_some()) {
                choices.push(Choice::RenameExisting);
            }
        }
        if returning.iter().all(|r| suffixed(&r.home, &s, 1).is_some()) {
            choices.push(Choice::RenameReturning);
        }
    }
    Ok(Some(Conflict {
        journal_id: entry.id,
        returning,
        occupants,
        choices,
        limits,
    }))
}

fn occupant(path: &str) -> Option<Occupant> {
    let md = fs::symlink_metadata(path).ok()?;
    Some(Occupant {
        path: path.to_string(),
        proof: Proof::of(&md)?,
    })
}

/// Add a decision to the entry's history; if the journal cannot take it,
/// `e` says so as well.
fn noted(
    db: &Db,
    id: i64,
    kind: &str,
    text: &str,
    note: &ConflictNote,
    e: anyhow::Error,
) -> anyhow::Error {
    match db.journal_event_conflict(id, "undo", kind, text, note) {
        Ok(()) => e,
        Err(pe) => e.context(pc_core::tf!(
            "журнал не принял запись об этом ({0}); запись {1} не дополнена",
            "the journal did not take the record of this ({0}); entry {1} was not updated",
            format!("{pe:#}"),
            id
        )),
    }
}

/// Carry out `choice` on the conflict `c` an undo of `entry` met.
pub(crate) fn resolve(
    db: &Db,
    entry: &JournalEntry,
    list: Vec<Moved>,
    c: Conflict,
    choice: Choice,
) -> Result<Tally> {
    let id = entry.id;
    if !c.choices.contains(&choice) {
        let why = pc_core::tf!(
            "{0}; выбор «{1}» здесь недоступен{2}; ничего не перенесено",
            "{0}; the choice “{1}” is not offered here{2}; nothing was moved",
            c.describe(),
            choice.as_str(),
            if c.limits.is_empty() {
                String::new()
            } else {
                format!(" ({})", c.limits.join("; "))
            }
        );
        return Err(noted(
            db,
            id,
            "refused",
            &why,
            &c.note(choice),
            anyhow!("{why}"),
        ));
    }
    match choice {
        Choice::Keep => {
            let why = c.kept_words();
            let note = c.note(choice);
            Err(noted(
                db,
                id,
                "kept",
                &why,
                &note,
                ConflictKept {
                    conflict: c,
                    why: why.clone(),
                }
                .into(),
            ))
        }
        Choice::RenameReturning => returning_renamed(db, entry, list, &c),
        Choice::Replace | Choice::RenameExisting => aside_then_back(db, entry, list, &c, choice),
    }
}

/// Choice 4: the unit comes back under the first free name, the existing
/// file is not touched.
fn returning_renamed(
    db: &Db,
    entry: &JournalEntry,
    mut list: Vec<Moved>,
    c: &Conflict,
) -> Result<Tally> {
    let id = entry.id;
    // A row written without evidence: the files at its own recorded
    // quarantine paths are what it has, and their evidence is written down
    // before anything moves (el-1y8uo B4).
    recovery::adopt_held_evidence(db, id, &mut list)?;
    let base = recovery::pairs_of(entry, list);
    let s = stem(&c.returning[0].home);
    for n in 1..=MAX_SUFFIX {
        let names: Option<Vec<String>> = base.iter().map(|p| suffixed(&p.rec.src, &s, n)).collect();
        let Some(names) = names else { break };
        // An early answer only; the rename itself is the test.
        if names.iter().any(|t| fs::symlink_metadata(t).is_ok()) {
            continue;
        }
        let pairs: Vec<Pair> = base
            .iter()
            .zip(&names)
            .map(|(p, t)| {
                let mut p = p.clone();
                p.look.src = t.clone();
                p
            })
            .collect();
        let items: Vec<Item> = pairs.iter().map(recovery::item_of).collect();
        if items.iter().any(|i| i.standing != Standing::Moved) {
            // A free name taken since the look above: the next one. Any
            // other doubt is about the unit itself, not the name — refused
            // with where everything is, nothing moved.
            if names.iter().any(|t| fs::symlink_metadata(t).is_ok()) {
                continue;
            }
            let why = pc_core::tf!(
                "undo: ничего не перенесено — кадр со спутниками возвращается только целиком, \
                 а не всё доказано: {0}",
                "undo: nothing was moved — the frame and its companions come back only \
                 together, and not all of them are proven: {0}",
                recovery::listing(&items)
            );
            return Err(noted(
                db,
                id,
                "refused",
                &why,
                &c.note(Choice::RenameReturning),
                anyhow!("{why}"),
            ));
        }
        // Recorded before the rename: a retry after an interruption looks
        // for what already came back under these names, by its evidence.
        let mut note = c.note(Choice::RenameReturning);
        note.returned_as = pairs
            .iter()
            .map(|p| ReturnedAs {
                src: p.rec.src.clone(),
                dst: p.rec.dst.clone(),
                to: p.look.src.clone(),
            })
            .collect();
        let text = pc_core::tf!(
            "возвращается под свободным именем: {0}",
            "coming back under a free name: {0}",
            names.join("; ")
        );
        db.journal_event_conflict(id, "undo", "attempt", &text, &note)?;
        match recovery::move_back(&pairs) {
            Back::Done(arrived, came) => {
                return recovery::finish(db, entry, &pairs, arrived, came, Tally::default())
            }
            Back::Failed(Unit::Refused(e)) if crate::is_taken(&e) => continue,
            Back::Failed(Unit::PutBack(pb))
                if pb.whole && !pb.told.folder_moved && crate::is_taken(&pb.error) =>
            {
                continue
            }
            Back::Failed(unit) => return Err(recovery::unit_failed(db, id, "undo", &items, unit)),
        }
    }
    let why = pc_core::tf!(
        "{0}; свободного имени до _{1} не нашлось; ничего не перенесено, файл остаётся в \
         карантине: {2}",
        "{0}; no free name up to _{1}; nothing was moved, the file stays in quarantine: {2}",
        c.describe(),
        MAX_SUFFIX,
        c.returning[0].held
    );
    Err(noted(
        db,
        id,
        "refused",
        &why,
        &c.note(Choice::RenameReturning),
        anyhow!("{why}"),
    ))
}

/// Choices 2 and 3: the existing unit is set aside first — into quarantine,
/// or to a free name beside it — under an entry of its own; then the place
/// is free, and the undo is the ordinary one, by evidence.
fn aside_then_back(
    db: &Db,
    entry: &JournalEntry,
    mut list: Vec<Moved>,
    c: &Conflict,
    choice: Choice,
) -> Result<Tally> {
    let id = entry.id;
    recovery::adopt_held_evidence(db, id, &mut list)?;
    let (aside_id, moved) = match set_aside(db, entry, c, choice) {
        Ok(x) => x,
        Err(e) => {
            let why = format!("{e:#}");
            return Err(noted(db, id, "refused", &why, &c.note(choice), e));
        }
    };
    let done = Tally {
        set_aside: moved.len() as u64,
        ..Default::default()
    };
    let mut note = c.note(choice);
    note.aside_entry = Some(aside_id);
    for o in &mut note.occupants {
        if let Some(m) = moved.iter().find(|m| m.src == o.src) {
            o.dst = m.dst.clone();
        }
    }
    let text = pc_core::tf!(
        "место освобождено записью {0}, её можно отменить: {1}",
        "the place was made free by entry {0}, which can be undone: {1}",
        aside_id,
        moved
            .iter()
            .map(|m| format!("{} → {}", m.src, m.dst))
            .collect::<Vec<_>>()
            .join("; ")
    );
    if let Err(e) = db.journal_event_conflict(id, "undo", "set-aside", &text, &note) {
        return Err(stop_run(e.context(text), &done, Route::Restore, Vec::new()));
    }
    let entry = db
        .journal_entry(id)?
        .with_context(|| pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", id))?;
    let pairs = recovery::pairs_of(&entry, list);
    match recovery::walk_back(db, id, "undo", &pairs) {
        Ok((arrived, came)) => recovery::finish(db, &entry, &pairs, arrived, came, done),
        // What was set aside stays set aside, under its own undoable entry;
        // asking again brings this one back once its place is free.
        Err(e) => Err(stop_run(e, &done, Route::Restore, Vec::new())),
    }
}

/// Move the occupants of `c` out of the way, as one unit under a new
/// journal entry of the entry's run: `quarantine-file` into the quarantine
/// beside their folder (choice 2), `set-aside` to a free name beside them
/// (choice 3). The id of that entry and what it moved.
fn set_aside(
    db: &Db,
    entry: &JournalEntry,
    c: &Conflict,
    choice: Choice,
) -> Result<(i64, Vec<Moved>)> {
    let s = stem(&c.returning[0].home);
    let (op, first) = match choice {
        Choice::Replace => ("quarantine-file", 0),
        _ => ("set-aside", 1),
    };
    let targets = |n: u32| -> Result<Option<Vec<String>>> {
        let mut out = Vec::new();
        for o in &c.occupants {
            let t = match choice {
                Choice::Replace => {
                    let q = crate::quarantine_target_for(&o.path, None)?.dst;
                    let q = q.to_string_lossy().into_owned();
                    if n == 0 {
                        Some(q)
                    } else {
                        suffixed(&q, &s, n)
                    }
                }
                _ => suffixed(&o.path, &s, n),
            };
            match t {
                Some(t) => out.push(t),
                None => return Ok(None),
            }
        }
        Ok(Some(out))
    };
    let mut jid = None;
    for n in first..=MAX_SUFFIX {
        let Some(names) = targets(n)? else { break };
        // An early answer only; the rename itself is the test.
        if names.iter().any(|t| fs::symlink_metadata(t).is_ok()) {
            continue;
        }
        let planned: Vec<Moved> = c
            .occupants
            .iter()
            .zip(&names)
            .map(|(o, t)| Moved {
                src: o.path.clone(),
                dst: t.clone(),
                proof: Some(o.proof.clone()),
            })
            .collect();
        let j = match jid {
            None => {
                let j = db.journal_begin(&pc_db::NewJournalEntry {
                    run_id: entry.run_id,
                    op,
                    target_id: None,
                    src: &planned[0].src,
                    dst: Some(&planned[0].dst),
                    size: crate::files::moved_bytes(&planned) as i64,
                    file_count: planned.len() as i64,
                    manifest: &planned,
                })?;
                jid = Some(j);
                j
            }
            Some(j) => {
                db.journal_retarget(j, &planned[0].dst, &planned)?;
                j
            }
        };
        // Bound to the occupants exactly as read when the choice was
        // offered: one replaced or changed since is refused, not moved.
        let members: Vec<Member> = planned.iter().map(Member::forward).collect();
        match move_unit(&members, Way::Forward, None, &[]) {
            Unit::Moved(arrived) => {
                let text = pc_core::tf!(
                    "отложено, чтобы вернуть запись {0} на её место",
                    "set aside so that entry {0} can come back to its place",
                    entry.id
                );
                let closed = db.journal_close(
                    j,
                    JournalStatus::Done,
                    &pc_db::Event {
                        text: &text,
                        moved: &planned,
                        ..pc_db::Event::new("forward", "done")
                    },
                );
                drop(arrived);
                if let Err(e) = closed {
                    let done = Tally {
                        set_aside: planned.len() as u64,
                        ..Default::default()
                    };
                    let e = e.context(pc_core::tf!(
                        "файлы отложены, но журнал не дописан: запись {0} осталась незавершённой — \
                         сверьте её",
                        "the files were set aside, but the journal was not completed: entry {0} \
                         is left pending — reconcile it",
                        j
                    ));
                    let e = stop_run(e, &done, Route::Restore, Vec::new());
                    return Err(crate::outcome::left_pending(e, j, Route::Restore));
                }
                return Ok((j, planned));
            }
            Unit::Refused(e) if crate::is_taken(&e) => continue,
            Unit::PutBack(pb)
                if pb.whole && !pb.told.folder_moved && crate::is_taken(&pb.error) =>
            {
                continue
            }
            unit => {
                let r = crate::unit::close_forward(db, j, "forward", Route::Restore, unit)?;
                let e = match r.stop {
                    Some(stop) => anyhow::Error::from(stop),
                    None => anyhow!("{}", r.why),
                };
                return Err(crate::outcome::with_placed(e, r.placed, Route::Restore));
            }
        }
    }
    let why = pc_core::tf!(
        "свободного имени до _{0} не нашлось; существующий файл не тронут: {1}",
        "no free name up to _{0}; the existing file was not touched: {1}",
        MAX_SUFFIX,
        c.occupants[0].path
    );
    if let Some(j) = jid {
        if let Err(pe) = crate::close_refused(db, j, "forward", &why, &Told::default()) {
            return Err(crate::outcome::left_pending(
                pe.context(why),
                j,
                Route::Restore,
            ));
        }
    }
    Err(anyhow!("{why}"))
}
