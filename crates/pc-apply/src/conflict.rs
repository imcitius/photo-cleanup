//! An undo whose original place is taken: the user's choice (el-14vx0).
//!
//! Recovery brings a file back only to a free place (§11.2): another file at
//! its name — or one that cannot be proven to be it — keeps the entry open
//! and the file in quarantine. That stays the default. The user decided
//! (2026-10-03, narrowed 2026-10-10 to the two choices that never touch the
//! existing file) that they may also choose, per conflict and for all the
//! remaining ones at once:
//!
//! 1. **keep** it in quarantine and return it by hand later — the refusal
//!    names its exact place in quarantine and where it belongs;
//! 2. **return the unit under a free name** (`IMG_1.CR2`, `_2`, …) and leave
//!    the existing file alone.
//!
//! The existing file is never moved, renamed, replaced or deleted by any of
//! this. What comes back is a whole unit (el-3wizg): the returning frame
//! with its companions, which take their frame's new name.
//!
//! A free name is never looked for and then taken: every move here is the
//! one bound, no-replace rename of [`crate::unit::move_unit`], and a name
//! that turns out taken at the rename ([`crate::Taken`]) — a file created
//! there between the look and the move — moves nothing and the next name is
//! tried. A look before the rename only skips names already taken.
//!
//! The preview is binding (el-14vx0, director decisions after el-zvg9s and
//! el-66rxn). What the preview read for an entry — [`Seen`]: a unit free to
//! come back, a conflict with the choice made on it, or a unit that stays —
//! is what an undo or a reconciliation of it is bound to, whatever was
//! chosen. Immediately before anything moves the entry is read again; any
//! difference on either side — a member changed, vanished, renamed, a
//! companion more beside the frame coming back or beside the existing one,
//! a place that became free or taken — refuses that unit: nothing of it
//! moves, the refusal is written down as `changed-since-preview`, and the
//! person is told to preview again. It never falls back to another choice,
//! another name or an ordinary undo.
//!
//! A unit with any member journaled before evidence was kept is only ever
//! kept — whether its place is taken or free: nothing proves which file is
//! which, so nothing of it is moved by an undo or a reconciliation.
//!
//! Every decision and its outcome — kept, renamed-returning, refused,
//! changed-since-preview — is appended to the entry's history as a
//! structured event ([`pc_db::ConflictNote`] with its `outcome`), and is part
//! of the typed result ([`Decision`], in [`Tally::decisions`] or carried by
//! the error, [`outcome_of`]); the command line and the web render the same.
//! A return under free names is also recorded *before* its rename, so a
//! retry after an interruption finds what already came back there — by its
//! evidence ([`crate::recovery`]).

use anyhow::{anyhow, Result};
use pc_core::proof::Proof;
use pc_db::{ConflictNote, Db, JournalEntry, JournalStatus, Moved, ReturnedAs};
use std::fs;
use std::path::Path;

use crate::outcome::{stopped_run, Halted};

use crate::recovery::{self, Back, Item, Pair, Spot, Standing};
use crate::unit::Unit;
use crate::{organize, Route, Tally};

/// How far the free names go: `_1` to `_999`. Past that, the file stays
/// where it is and the refusal says so.
const MAX_SUFFIX: u32 = 999;

/// What to do with an undo whose original place is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Choice {
    /// Leave it in quarantine; the default.
    Keep,
    /// Return this unit under a free name; the existing one stays.
    RenameReturning,
}

impl Choice {
    pub const ALL: [Choice; 2] = [Choice::Keep, Choice::RenameReturning];

    /// The word the command line takes (`--on-conflict`) and the web sends.
    pub fn as_str(self) -> &'static str {
        match self {
            Choice::Keep => "keep",
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
            Choice::RenameReturning => pc_core::tr!(
                "вернуть под именем *_1, существующий не трогать",
                "return it as *_1 and leave the existing file alone"
            ),
        }
    }
}

/// How a decision ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// Left in quarantine, as chosen — or as the only thing a unit without
    /// evidence may do; nothing moved.
    Kept,
    /// This one came back under a free name; the existing one stayed.
    RenamedReturning,
    /// Not carried out — not offered, a member unproven, no free name, a
    /// move refused; the words say which. Nothing of the unit moved unless
    /// the result says so.
    Refused,
    /// What is there is not what the preview showed; nothing moved, and the
    /// person is asked to preview again.
    ChangedSincePreview,
}

impl Outcome {
    pub const ALL: [Outcome; 4] = [
        Outcome::Kept,
        Outcome::RenamedReturning,
        Outcome::Refused,
        Outcome::ChangedSincePreview,
    ];

    /// The word in the journal's structured history.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Kept => "kept",
            Outcome::RenamedReturning => "renamed-returning",
            Outcome::Refused => "refused",
            Outcome::ChangedSincePreview => "changed-since-preview",
        }
    }

    /// In words, for a person; the same on the command line and the web.
    pub fn words(self) -> &'static str {
        match self {
            Outcome::Kept => pc_core::tr!("оставлено в карантине", "kept"),
            Outcome::RenamedReturning => {
                pc_core::tr!("возвращено под свободным именем", "renamed-returning")
            }
            Outcome::Refused => pc_core::tr!("отказано", "refused"),
            Outcome::ChangedSincePreview => pc_core::tr!(
                "изменилось с предпросмотра — обновите предпросмотр",
                "changed since the preview — refresh the preview"
            ),
        }
    }
}

/// One decision on an entry the preview showed and how it ended: what the
/// typed result of an undo or a reconciliation carries for it, alongside
/// the event in the entry's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub journal_id: i64,
    /// The choice made on a taken place; `None` when there was none to make
    /// — the place was free, or the unit could not come back at all.
    pub choice: Option<Choice>,
    pub outcome: Outcome,
}

/// An undo or reconciliation that ended in a decision other than a move —
/// kept, refused, changed since the preview — typed: the decision, and the
/// words of why.
#[derive(Debug)]
pub struct Decided {
    pub decision: Decision,
    pub cause: anyhow::Error,
}

impl std::fmt::Display for Decided {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.cause)
    }
}

// No `source()`: the cause is already in the words above.
impl std::error::Error for Decided {}

fn decided_in(e: &anyhow::Error) -> Option<&Decided> {
    if let Some(d) = e.chain().find_map(|c| c.downcast_ref::<Decided>()) {
        return Some(d);
    }
    e.downcast_ref::<Halted>()
        .and_then(|h| decided_in(&h.cause))
}

/// What an undo or a reconciliation that ended in `e` did, typed: the work
/// done before it stopped, if any, and the decision it ended in — for the
/// command line and the web alike.
pub fn outcome_of(e: &anyhow::Error) -> Tally {
    let mut t = stopped_run(e).map(|s| s.done.clone()).unwrap_or_default();
    if let Some(d) = decided_in(e) {
        if !t.decisions.contains(&d.decision) {
            t.decisions.push(d.decision.clone());
        }
    }
    t
}

/// `e`, carrying `d`: kept with the work it reports, if it reports any (a
/// stop of the run stays one), and typed as [`Decided`] otherwise.
fn carrying(e: anyhow::Error, d: Decision) -> anyhow::Error {
    if stopped_run(&e).is_some() || crate::is_run_stop(&e) {
        let t = Tally {
            decisions: vec![d],
            ..Default::default()
        };
        return crate::stop_run(e, &t, Route::Restore, Vec::new());
    }
    Decided {
        decision: d,
        cause: e,
    }
    .into()
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
    /// In such a row, the evidence of what is held at its recorded
    /// quarantine path, read when the entry was previewed: a change to it
    /// since is a different preview. Such a unit is only ever kept, so this
    /// is part of the snapshot a decision is bound to, never a reason to
    /// move anything. `None` when the journal recorded evidence.
    pub seen: Option<Proof>,
}

impl Returning {
    fn moved(&self) -> Moved {
        Moved {
            src: self.home.clone(),
            dst: self.held.clone(),
            proof: self.proof.clone().or_else(|| self.seen.clone()),
        }
    }
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
    /// or a companion's place alone. Never moved by any choice; read so that
    /// a choice holds only while they are what was shown.
    pub occupants: Vec<Occupant>,
    /// Files beside the unit coming back, named as its companions, that are
    /// not part of the entry, as read: never moved, and bound like the rest.
    pub beside: Vec<Occupant>,
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
        if self.choices == [Choice::Keep] {
            return pc_core::tf!(
                "{0}; оставлено в карантине: {1} — место, куда возвращать: {2}. Ничего не \
                 перенесено, запись остаётся открытой ({3})",
                "{0}; kept in quarantine: {1} — it belongs at {2}. Nothing was moved and the \
                 entry stays open ({3})",
                self.describe(),
                r.held,
                r.home,
                self.limits.join("; ")
            );
        }
        pc_core::tf!(
            "{0}; оставлено в карантине: {1} — место, куда возвращать: {2}. Ничего не перенесено, \
             запись остаётся открытой; выберите «вернуть под именем *_1», чтобы вернуть его рядом \
             с существующим (`--on-conflict rename-returning` или диалог на странице «Карантин»)",
            "{0}; kept in quarantine: {1} — it belongs at {2}. Nothing was moved and the entry \
             stays open; choose “return as *_1” to bring it back beside the existing file \
             (`--on-conflict rename-returning`, or the dialog on the Quarantine page)",
            self.describe(),
            r.held,
            r.home
        )
    }

    fn note(&self, choice: Option<Choice>) -> ConflictNote {
        ConflictNote {
            choice: choice.map(|c| c.as_str().to_string()).unwrap_or_default(),
            returning: self.returning.iter().map(Returning::moved).collect(),
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
            outcome: None,
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
    e.chain()
        .find_map(|c| c.downcast_ref::<ConflictKept>())
        .or_else(|| decided_in(e).and_then(|d| conflict_kept(&d.cause)))
}

/// A unit that does not come back, whatever is chosen, as read: the members
/// as they were found and why, in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub journal_id: i64,
    /// A member was journaled before evidence was kept: the unit is kept,
    /// never moved (user decision 2026-10-10).
    pub legacy: bool,
    /// The members with where each was found, and anything beside the
    /// frame that is not part of the unit — in words.
    pub why: String,
    /// Where the frame is held and where it belongs.
    pub held: String,
    pub home: String,
}

/// One member of a unit free to come back, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub home: String,
    pub held: String,
    pub proof: Option<Proof>,
    /// Already at home, proven; nothing to move.
    pub at_home: bool,
}

/// What a preview read for one entry, and what its undo or reconciliation
/// is then bound to (el-14vx0): read again immediately before anything
/// moves, it must be exactly this, or the unit is refused as changed since
/// the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// Every member proven — at home, or held with its place free — and
    /// nothing beside the frame that is not part of it: it comes back.
    Free {
        journal_id: i64,
        places: Vec<Place>,
        /// Files beside a held member, named as its companions, that are
        /// not part of the entry ([`strangers`]), as read: they stay where
        /// they are, and one more, one fewer or one changed is a different
        /// reading (R3-B1).
        beside: Vec<Occupant>,
    },
    /// Its place is taken by another file: the choice is the person's.
    Taken(Conflict),
    /// It does not come back.
    Held(Held),
}

impl Seen {
    /// The journal entry this reading is of.
    pub fn journal_id(&self) -> i64 {
        match self {
            Seen::Free { journal_id, .. } => *journal_id,
            Seen::Taken(c) => c.journal_id,
            Seen::Held(h) => h.journal_id,
        }
    }

    pub fn conflict(&self) -> Option<&Conflict> {
        match self {
            Seen::Taken(c) => Some(c),
            _ => None,
        }
    }

    fn note(&self, choice: Option<Choice>) -> ConflictNote {
        match self {
            Seen::Taken(c) => c.note(choice),
            Seen::Free { places, .. } => ConflictNote {
                choice: String::new(),
                returning: places
                    .iter()
                    .map(|p| Moved {
                        src: p.home.clone(),
                        dst: p.held.clone(),
                        proof: p.proof.clone(),
                    })
                    .collect(),
                occupants: Vec::new(),
                returned_as: Vec::new(),
                outcome: None,
            },
            Seen::Held(h) => ConflictNote {
                choice: choice.map(|c| c.as_str().to_string()).unwrap_or_default(),
                returning: vec![Moved {
                    src: h.home.clone(),
                    dst: h.held.clone(),
                    proof: None,
                }],
                occupants: Vec::new(),
                returned_as: Vec::new(),
                outcome: None,
            },
        }
    }

    fn words(&self) -> String {
        match self {
            Seen::Free { places, .. } => {
                let back = places
                    .iter()
                    .map(|p| format!("{} ← {}", p.home, p.held))
                    .collect::<Vec<_>>()
                    .join("; ");
                pc_core::tf!(
                    "место было свободно, возвращалось: {0}",
                    "the place was free, coming back: {0}",
                    back
                )
            }
            Seen::Taken(c) => c.describe(),
            Seen::Held(h) => h.why.clone(),
        }
    }
}

/// A preview of an entry and the choice made on it: the choice counts only
/// on a taken place ([`Seen::Taken`]); for any other it is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewed {
    pub seen: Seen,
    pub choice: Choice,
}

impl Reviewed {
    /// What `seen` would do with nothing chosen: keep a taken place's file
    /// in quarantine.
    pub fn keep(seen: Seen) -> Reviewed {
        Reviewed {
            seen,
            choice: Choice::Keep,
        }
    }
}

/// What a preview of an undo of `entry` reads now. Reads only.
pub fn undo_seen(entry: &JournalEntry) -> Result<Seen> {
    if !recovery::undo_offered(entry) {
        return Ok(Seen::Held(not_offered(entry)));
    }
    seen_of(entry)
}

/// What a preview of the reconciliation of the interrupted `entry` reads
/// now: the same reading, and the same choices, as an undo (el-14vx0 B3).
/// Reads only.
pub fn reconcile_seen(entry: &JournalEntry) -> Result<Seen> {
    if entry.status != JournalStatus::Pending || entry.op.contains("purge") {
        return Ok(Seen::Held(not_offered(entry)));
    }
    seen_of(entry)
}

/// The conflict an undo of `entry` would meet now, if its only obstacle is
/// that its place is taken. Reads only.
pub fn undo_conflict(entry: &JournalEntry) -> Result<Option<Conflict>> {
    Ok(match undo_seen(entry)? {
        Seen::Taken(c) => Some(c),
        _ => None,
    })
}

/// The conflict a reconciliation of `entry` would meet now. Reads only.
pub fn reconcile_conflict(entry: &JournalEntry) -> Result<Option<Conflict>> {
    Ok(match reconcile_seen(entry)? {
        Seen::Taken(c) => Some(c),
        _ => None,
    })
}

fn not_offered(entry: &JournalEntry) -> Held {
    Held {
        journal_id: entry.id,
        legacy: false,
        why: pc_core::tf!(
            "запись {0} в состоянии «{1}»",
            "entry {0} is “{1}”",
            entry.id,
            entry.status.as_str()
        ),
        held: entry.dst.clone().unwrap_or_default(),
        home: entry.src.clone(),
    }
}

/// An undo of `entry` bound to its preview (el-14vx0): `reviewed` is what
/// the preview read for this entry and the choice made on it, `None` when
/// the preview did not show it. What is read now must be exactly that, and
/// it is then carried out — the unit comes back, the choice on a taken
/// place is made, a unit that does not come back is kept or refused;
/// anything else is refused as changed since the preview, nothing moved and
/// the refusal written down.
pub fn undo_reviewed(db: &Db, journal_id: i64, reviewed: Option<&Reviewed>) -> Result<Tally> {
    let entry = recovery::undoable(db, journal_id)?;
    let now = seen_of(&entry)?;
    carry(db, &entry, reviewed, now)
}

/// [`crate::reconcile_undo`] bound to a preview, as [`undo_reviewed`].
pub fn reconcile_reviewed(
    db: &Db,
    journal_id: i64,
    reviewed: Option<&Reviewed>,
) -> Result<recovery::Reconciled> {
    let entry = recovery::reconcilable(db, journal_id)?;
    let items = recovery::reconcile(db, journal_id)?;
    let now = seen_of(&entry)?;
    let done = carry(db, &entry, reviewed, now)?;
    Ok(recovery::Reconciled { items, done })
}

/// An undo of `journal_id` as read now, nothing chosen: what
/// [`crate::undo`] is. The same path as a reviewed one, so a taken place
/// keeps its file and says so, and every outcome is typed alike.
pub(crate) fn undo_now(db: &Db, journal_id: i64) -> Result<Tally> {
    let entry = recovery::undoable(db, journal_id)?;
    let now = seen_of(&entry)?;
    carry(db, &entry, Some(&Reviewed::keep(now.clone())), now)
}

/// A reconciliation of `journal_id` as read now: what
/// [`crate::reconcile_undo`] is.
pub(crate) fn reconcile_now(db: &Db, journal_id: i64) -> Result<recovery::Reconciled> {
    let entry = recovery::reconcilable(db, journal_id)?;
    let items = recovery::reconcile(db, journal_id)?;
    let now = seen_of(&entry)?;
    let done = carry(db, &entry, Some(&Reviewed::keep(now.clone())), now)?;
    Ok(recovery::Reconciled { items, done })
}

/// Carry out what was reviewed on `entry`, which reads `now` as `now`.
fn carry(db: &Db, entry: &JournalEntry, reviewed: Option<&Reviewed>, now: Seen) -> Result<Tally> {
    let Some(r) = reviewed.filter(|r| r.seen == now) else {
        return Err(changed(db, entry, reviewed, &now));
    };
    match now {
        Seen::Taken(c) => resolve(db, entry, c, r.choice),
        Seen::Held(h) => Err(held(db, entry, &r.seen, &h)),
        Seen::Free { .. } => free(db, entry, &r.seen),
    }
}

/// A unit free to come back: back, by its evidence, as one unit. A refusal
/// on the way — the preview's reading is checked again by the move itself —
/// is written down as this entry's outcome.
fn free(db: &Db, entry: &JournalEntry, seen: &Seen) -> Result<Tally> {
    let phase = phase_of(entry);
    let pairs = recovery::pairs_of(entry, list_for(entry)?);
    let known = match seen {
        Seen::Free { beside, .. } => beside.as_slice(),
        _ => &[],
    };
    match recovery::walk_back(db, entry.id, phase, &pairs, known) {
        Ok((arrived, came)) => {
            if entry.status == JournalStatus::Pending {
                recovery::reconcile_close(db, entry, arrived, came)
            } else {
                recovery::finish(db, entry, &pairs, arrived, came, None)
            }
        }
        Err(e) => {
            let why = format!("{e:#}");
            Err(noted(
                db,
                entry,
                None,
                Outcome::Refused,
                &why,
                seen.note(None),
                e,
            ))
        }
    }
}

/// A unit that does not come back, as previewed: kept, if it has no
/// evidence, refused otherwise — written down and typed.
fn held(db: &Db, entry: &JournalEntry, seen: &Seen, h: &Held) -> anyhow::Error {
    if h.legacy {
        noted(
            db,
            entry,
            Some(Choice::Keep),
            Outcome::Kept,
            &h.why,
            seen.note(Some(Choice::Keep)),
            anyhow!("{}", h.why),
        )
    } else {
        noted(
            db,
            entry,
            None,
            Outcome::Refused,
            &h.why,
            seen.note(None),
            anyhow!("{}", h.why),
        )
    }
}

/// The refusal of a unit whose reading is not the one the preview showed
/// (`r`, or none): written down with the choice made, what was seen and
/// what is there now, typed as [`Outcome::ChangedSincePreview`]. Nothing is
/// moved.
fn changed(db: &Db, entry: &JournalEntry, r: Option<&Reviewed>, now: &Seen) -> anyhow::Error {
    let then = match r {
        Some(r) => r.seen.words(),
        None => pc_core::tr!(
            "предпросмотр эту запись не показывал",
            "the preview did not show this entry"
        )
        .to_string(),
    };
    let why = pc_core::tf!(
        "запись {0}: с предпросмотра на исходном месте или в карантине что-то изменилось; \
         ничего не перенесено — обновите предпросмотр и выберите снова. Было: {1}. Сейчас: {2}",
        "entry {0}: something changed at the original place or in quarantine since the preview; \
         nothing was moved — refresh the preview and choose again. Then: {1}. Now: {2}",
        entry.id,
        then,
        now.words()
    );
    let choice = r.and_then(|r| r.seen.conflict().map(|_| r.choice));
    let note = match r {
        Some(r) => r.seen.note(choice),
        None => now.note(None),
    };
    noted(
        db,
        entry,
        choice,
        Outcome::ChangedSincePreview,
        &why,
        note,
        anyhow!("{why}"),
    )
}

/// What `entry` reads as now: free, taken or held.
fn seen_of(entry: &JournalEntry) -> Result<Seen> {
    let list = match list_for(entry) {
        Ok(list) => list,
        Err(e) => {
            return Ok(Seen::Held(Held {
                journal_id: entry.id,
                legacy: false,
                why: format!("{e:#}"),
                held: entry.dst.clone().unwrap_or_default(),
                home: entry.src.clone(),
            }))
        }
    };
    let pairs = recovery::pairs_of(entry, list);
    if let Some(c) = of(entry, &pairs)? {
        return Ok(Seen::Taken(c));
    }
    let frame = frame_of(entry, &pairs);
    let (home, held) = pairs
        .get(frame)
        .map(|p| (p.look.src.clone(), p.look.dst.clone()))
        .unwrap_or_else(|| (entry.src.clone(), entry.dst.clone().unwrap_or_default()));
    let items: Vec<Item> = pairs.iter().map(recovery::item_of).collect();
    // A member journaled before evidence was kept: nothing proves which
    // file is which, so the unit is only ever kept (user decision
    // 2026-10-10, R3-B2) — even with its place free.
    if pairs.iter().any(|p| p.rec.proof.is_none()) {
        let why = pc_core::tf!(
            "запись {0} сделана старой версией без доказательств: файлы не возвращаются \
             автоматически, только «оставить». Оставлено в карантине: {1} — место, куда \
             возвращать: {2}; верните вручную. Файлы записи: {3}",
            "entry {0} was written by an older version without evidence: its files are not \
             brought back automatically, only “keep”. Kept in quarantine: {1} — it belongs at \
             {2}; return it by hand. The entry's files: {3}",
            entry.id,
            held,
            home,
            pairs
                .iter()
                .map(|p| format!("{} ← {}", p.look.src, p.look.dst))
                .collect::<Vec<_>>()
                .join("; ")
        );
        return Ok(Seen::Held(Held {
            journal_id: entry.id,
            legacy: true,
            why,
            held,
            home,
        }));
    }
    let Some(beside) = beside(entry, &pairs) else {
        return Ok(Seen::Held(Held {
            journal_id: entry.id,
            legacy: false,
            why: pc_core::tf!(
                "запись {0}: файл рядом с возвращающимся кадром не прочитать; ничего не перенесено",
                "entry {0}: a file beside the frame coming back cannot be read; nothing was moved",
                entry.id
            ),
            held,
            home,
        }));
    };
    if items.iter().any(|i| i.why().is_some()) {
        let listing = recovery::listing(&items);
        let why = if entry.status == JournalStatus::Pending {
            pc_core::tf!(
                "сверка: ничего не перенесено — кадр со спутниками возвращается только целиком, \
                 а не всё доказано: {0}",
                "reconcile: nothing was moved — the frame and its companions come back only \
                 together, and not all of them are proven: {0}",
                listing
            )
        } else {
            pc_core::tf!(
                "undo: ничего не перенесено — кадр со спутниками возвращается только целиком, а \
                 не всё доказано: {0}",
                "undo: nothing was moved — the frame and its companions come back only together, \
                 and not all of them are proven: {0}",
                listing
            )
        };
        return Ok(Seen::Held(Held {
            journal_id: entry.id,
            legacy: false,
            why,
            held,
            home,
        }));
    }
    Ok(Seen::Free {
        journal_id: entry.id,
        beside,
        places: pairs
            .iter()
            .zip(&items)
            .map(|(p, i)| Place {
                home: p.look.src.clone(),
                held: p.look.dst.clone(),
                proof: p.rec.proof.clone(),
                at_home: i.standing == Standing::Home,
            })
            .collect(),
    })
}

/// The list `entry` is walked back from: an undo's for a finished entry, a
/// reconciliation's for an interrupted one.
fn list_for(entry: &JournalEntry) -> Result<Vec<Moved>> {
    match entry.status {
        JournalStatus::Done => recovery::list_of(entry),
        JournalStatus::Pending => recovery::pending_list(entry),
        s => Err(anyhow!(pc_core::tf!(
            "запись {0} в состоянии «{1}»",
            "entry {0} is “{1}”",
            entry.id,
            s.as_str()
        ))),
    }
}

/// The history phase a decision on `entry` is written under.
fn phase_of(entry: &JournalEntry) -> &'static str {
    if entry.status == JournalStatus::Pending {
        "reconcile"
    } else {
        "undo"
    }
}

fn frame_of(entry: &JournalEntry, pairs: &[Pair]) -> usize {
    pairs
        .iter()
        .position(|p| p.rec.src == entry.src)
        .unwrap_or(0)
}

/// Files beside a member of the unit that is held away from home, named as
/// its companions (`.xmp`, `.aae`, `._*`), that are not members (el-14vx0
/// R3-B1). One may have been there before the operation — another
/// photograph's sidecar of the same name, which never travels with this one
/// (see `an_undo_brings_back_only_what_this_move_took`); nothing proves
/// whose it is, so it is never moved. What the preview found is what the
/// unit is bound to: an `.aae` that appears beside the frame after the
/// preview is a different reading, and the unit does not come back. A
/// folder has no companions.
fn strangers(entry: &JournalEntry, pairs: &[Pair]) -> Vec<String> {
    if entry.op == "quarantine" {
        return Vec::new();
    }
    let ours: Vec<&str> = pairs.iter().map(|p| p.look.dst.as_str()).collect();
    let mut out: Vec<String> = Vec::new();
    for p in pairs {
        if fs::symlink_metadata(&p.look.dst).is_err() {
            continue;
        }
        for side in crate::files::companions(Path::new(&p.look.dst)) {
            let side = side.to_string_lossy().into_owned();
            if !ours.contains(&side.as_str()) && !out.contains(&side) {
                out.push(side);
            }
        }
    }
    out
}

/// [`strangers`] with their evidence, as read; `None` if one cannot be read.
fn beside(entry: &JournalEntry, pairs: &[Pair]) -> Option<Vec<Occupant>> {
    strangers(entry, pairs)
        .iter()
        .map(|p| occupant(p))
        .collect()
}

/// Files left at the places a unit moved away from, named as companions of
/// its members — called once the unit has moved, when nothing of it is
/// there any more — that are not exactly the ones the preview read there
/// (`known`, by their evidence): anything else appeared while it moved.
pub(crate) fn left_behind(origins: &[String], known: &[Occupant]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for o in origins {
        for side in crate::files::companions(Path::new(o)) {
            let side = side.to_string_lossy().into_owned();
            if origins.contains(&side) || out.contains(&side) {
                continue;
            }
            let same = occupant(&side).is_some_and(|now| known.contains(&now));
            if !same {
                out.push(side);
            }
        }
    }
    out
}

pub(crate) fn stranger_words(strangers: &[String]) -> String {
    pc_core::tf!(
        "пока кадр возвращался, рядом с ним появился файл с именем его спутника: {0}; кадр \
         без него не возвращается, а чужой файл не переносится",
        "while the frame was coming back, a file named as its companion appeared beside it: \
         {0}; the frame does not come back without it, and a file that is not this entry's is \
         not moved",
        strangers.join("; ")
    )
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

/// The conflict, when every item of the entry is held where it belongs —
/// proven by its evidence, or at its own recorded quarantine path in a row
/// written without evidence — nothing beside it that is not part of it, and
/// some of their places bear something that is not proven to be the item.
/// Anything else — a member gone, changed, a stranger in quarantine, an
/// unreadable place, a return under free names already under way — is not
/// a choice to offer: recovery refuses it as before.
fn of(entry: &JournalEntry, pairs: &[Pair]) -> Result<Option<Conflict>> {
    let mut returning = Vec::new();
    let mut occupants: Vec<Occupant> = Vec::new();
    let frame = frame_of(entry, pairs);
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
        let seen = match proof {
            Some(_) => None,
            None => match fs::symlink_metadata(&p.look.dst)
                .ok()
                .and_then(|md| Proof::of(&md))
            {
                Some(e) => Some(e),
                None => return Ok(None),
            },
        };
        let r = Returning {
            home: p.look.src.clone(),
            held: p.look.dst.clone(),
            proof: p.look.proof.clone(),
            seen,
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
    // Files beside the unit coming back that are not part of it: read, so
    // that a choice holds only while they are as shown (R3-B1).
    let Some(beside) = beside(entry, pairs) else {
        return Ok(None);
    };
    // The existing frame's own companions are part of what bears the place:
    // read, so that a choice holds only while they are as shown. Nothing of
    // the existing unit is ever moved.
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
    // A member journaled before evidence was kept: nothing proves which
    // file is which, so the unit is not moved on a choice — only kept
    // (director decision after el-zvg9s). A unit is never split, so one
    // such member decides for all of it.
    if returning.iter().any(|r| r.proof.is_none()) {
        limits.push(
            pc_core::tr!(
                "запись сделана старой версией без доказательств: файлы не переносятся по \
                 выбору — только «оставить»; верните их вручную",
                "the entry was written by an older version without evidence: nothing is moved \
                 on a choice — only “keep”; return the files by hand"
            )
            .to_string(),
        );
    } else if !file_op {
        limits.push(
            pc_core::tr!(
                "папка возвращается только на своё место",
                "a folder comes back only to its own place"
            )
            .to_string(),
        );
    } else {
        // Lightroom's files too: the existing file is not touched, and the
        // one coming back takes a name of its own beside it (user decision
        // 2026-10-06, the same now for everyone).
        let s = stem(&home);
        if returning.iter().all(|r| suffixed(&r.home, &s, 1).is_some()) {
            choices.push(Choice::RenameReturning);
        }
    }
    Ok(Some(Conflict {
        journal_id: entry.id,
        returning,
        occupants,
        beside,
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

/// Add a decision that moved nothing to the entry's history — `kept`, or
/// `refused` with its outcome — and type `e` with it ([`Decided`]); if the
/// journal cannot take it, `e` says so as well.
fn noted(
    db: &Db,
    entry: &JournalEntry,
    choice: Option<Choice>,
    outcome: Outcome,
    text: &str,
    mut note: ConflictNote,
    e: anyhow::Error,
) -> anyhow::Error {
    note.outcome = Some(outcome.as_str().to_string());
    let kind = if outcome == Outcome::Kept {
        "kept"
    } else {
        "refused"
    };
    let e = match db.journal_event_conflict(entry.id, phase_of(entry), kind, text, &note) {
        Ok(()) => e,
        Err(pe) => e.context(pc_core::tf!(
            "журнал не принял запись об этом ({0}); запись {1} не дополнена",
            "the journal did not take the record of this ({0}); entry {1} was not updated",
            format!("{pe:#}"),
            entry.id
        )),
    };
    carrying(
        e,
        Decision {
            journal_id: entry.id,
            choice,
            outcome,
        },
    )
}

/// Carry out `choice` on the conflict `c` an undo or a reconciliation of
/// `entry` met — read immediately before, and exactly as reviewed. However
/// it ends, the decision is in the entry's history with its outcome and in
/// the typed result: the [`Tally`] of a choice carried out, the error
/// ([`outcome_of`]) of any other.
fn resolve(db: &Db, entry: &JournalEntry, c: Conflict, choice: Choice) -> Result<Tally> {
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
            entry,
            Some(choice),
            Outcome::Refused,
            &why,
            c.note(Some(choice)),
            anyhow!("{why}"),
        ));
    }
    if choice == Choice::Keep {
        let why = c.kept_words();
        let note = c.note(Some(choice));
        return Err(noted(
            db,
            entry,
            Some(choice),
            Outcome::Kept,
            &why,
            note,
            ConflictKept {
                conflict: c,
                why: why.clone(),
            }
            .into(),
        ));
    }
    returning_renamed(db, entry, &c)
}

/// The unit comes back under the first free name, the existing file is not
/// touched.
fn returning_renamed(db: &Db, entry: &JournalEntry, c: &Conflict) -> Result<Tally> {
    let choice = Choice::RenameReturning;
    let refused = |why: String, e: anyhow::Error| {
        noted(
            db,
            entry,
            Some(choice),
            Outcome::Refused,
            &why,
            c.note(Some(choice)),
            e,
        )
    };
    let id = entry.id;
    let base = recovery::pairs_of(entry, list_for(entry)?);
    let s = stem(&c.returning[0].home);
    for n in 1..=MAX_SUFFIX {
        let names: Option<Vec<String>> = base.iter().map(|p| suffixed(&p.rec.src, &s, n)).collect();
        let Some(names) = names else { break };
        // An early answer only; the rename itself is the test. A name whose
        // companions are already there — `IMG_1.xmp` of someone else's
        // `IMG_1.CR2` — is not free for this unit either.
        let busy = |t: &String| {
            fs::symlink_metadata(t).is_ok() || !crate::files::companions(Path::new(t)).is_empty()
        };
        if names.iter().any(busy) {
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
            return Err(refused(why.clone(), anyhow!("{why}")));
        }
        // Recorded before the rename: a retry after an interruption looks
        // for what already came back under these names, by its evidence.
        let mut note = c.note(Some(choice));
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
        if let Err(e) = db.journal_event_conflict(id, phase_of(entry), "attempt", &text, &note) {
            return Err(refused(format!("{e:#}"), e));
        }
        match recovery::move_back(&pairs, &c.beside) {
            Back::Done(arrived, came) => {
                note.outcome = Some(Outcome::RenamedReturning.as_str().to_string());
                let mut done = recovery::finish(db, entry, &pairs, arrived, came, Some(&note))?;
                done.decisions.push(Decision {
                    journal_id: id,
                    choice: Some(choice),
                    outcome: Outcome::RenamedReturning,
                });
                return Ok(done);
            }
            Back::Failed(Unit::Refused(e)) if crate::is_taken(&e) => continue,
            Back::Failed(Unit::PutBack(pb))
                if pb.whole && !pb.told.folder_moved && crate::is_taken(&pb.error) =>
            {
                continue
            }
            Back::Failed(unit) => {
                let e = recovery::unit_failed(db, id, phase_of(entry), &items, unit);
                return Err(refused(format!("{e:#}"), e));
            }
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
    Err(refused(why.clone(), anyhow!("{why}")))
}

/// Every entry an undo of run `run_id` would walk, as read now, in its
/// order: what the preview shows, and asks about, before the first file
/// moves (el-14vx0 B5). Reads only.
pub fn run_seen(db: &Db, run_id: i64) -> Result<Vec<Seen>> {
    db.journal_by_run_op(run_id, "organize")?
        .iter()
        .map(undo_seen)
        .collect()
}

/// Every conflict among [`run_seen`].
pub fn run_conflicts(db: &Db, run_id: i64) -> Result<Vec<Conflict>> {
    Ok(run_seen(db, run_id)?
        .into_iter()
        .filter_map(|s| match s {
            Seen::Taken(c) => Some(c),
            _ => None,
        })
        .collect())
}
