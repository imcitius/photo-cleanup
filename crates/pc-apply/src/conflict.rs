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
//! The preview is binding (el-14vx0, director decision after el-zvg9s): a
//! choice is made on a conflict as the preview read it — the unit coming
//! back, by its evidence, and the existing unit with every companion it
//! had, by theirs — and holds only while that is still what is there.
//! Immediately before the first move every member is read again; any
//! difference — something appeared, vanished, changed, was renamed, a
//! companion more — refuses that unit: nothing of it moves, the refusal is
//! written down as `changed-since-preview`, and the person is told to
//! preview again. It never falls back to another choice or another name;
//! a place that became free since is not a reason to bring the unit back
//! by an ordinary undo, and a place taken since the preview is not
//! decided for anyone.
//!
//! A unit with any member journaled before evidence was kept is offered
//! only "keep": nothing proves which file is which, so nothing of it is
//! moved on a choice.
//!
//! Lightroom is never touched (user decision 2026-10-06, invariant 7): when
//! any file of the conflict is in a live catalogue or on a Lightroom path,
//! only "keep" and "return under a free name" are offered — the existing
//! file is neither renamed nor carried away. A bundle comes back only to its
//! own place.
//!
//! Every decision and its outcome — kept, replaced, renamed-existing,
//! renamed-returning, refused, changed-since-preview — is appended to the
//! entry's history as a structured event ([`pc_db::ConflictNote`] with its
//! `outcome`), and is part of the typed result ([`Decision`], in
//! [`Tally::decisions`] or carried by the error, [`outcome_of`]); the
//! command line and the web render the same. A return under free names is
//! also recorded *before* its rename, so a retry after an interruption
//! finds what already came back there — by its evidence
//! ([`crate::recovery`]). A retry after a stop between setting the
//! occupants aside and the return finds the place free and is an ordinary
//! undo.

use anyhow::{anyhow, Context, Result};
use pc_core::proof::{Kind, Proof};
use pc_db::{ConflictNote, Db, JournalEntry, JournalStatus, Moved, ReturnedAs};
use std::fs;
use std::path::Path;

use crate::outcome::{stopped_run, Halted};

use crate::located::{Told, Way};
use crate::recovery::{self, Back, Item, Pair, Spot, Standing};
use crate::unit::{move_unit_checked, Member, Unit};
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

/// How a decision on a taken place ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// Left in quarantine, as chosen; nothing moved.
    Kept,
    /// The existing unit went to quarantine and this one came home.
    Replaced,
    /// The existing unit took a free name and this one came home.
    RenamedExisting,
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
    pub const ALL: [Outcome; 6] = [
        Outcome::Kept,
        Outcome::Replaced,
        Outcome::RenamedExisting,
        Outcome::RenamedReturning,
        Outcome::Refused,
        Outcome::ChangedSincePreview,
    ];

    /// The word in the journal's structured history.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Kept => "kept",
            Outcome::Replaced => "replaced",
            Outcome::RenamedExisting => "renamed-existing",
            Outcome::RenamedReturning => "renamed-returning",
            Outcome::Refused => "refused",
            Outcome::ChangedSincePreview => "changed-since-preview",
        }
    }

    /// In words, for a person; the same on the command line and the web.
    pub fn words(self) -> &'static str {
        match self {
            Outcome::Kept => pc_core::tr!("оставлено в карантине", "kept"),
            Outcome::Replaced => pc_core::tr!("заменено", "replaced"),
            Outcome::RenamedExisting => {
                pc_core::tr!("существующий переименован", "renamed-existing")
            }
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

    /// The outcome of carrying out `choice`.
    fn of(choice: Choice) -> Outcome {
        match choice {
            Choice::Keep => Outcome::Kept,
            Choice::Replace => Outcome::Replaced,
            Choice::RenameExisting => Outcome::RenamedExisting,
            Choice::RenameReturning => Outcome::RenamedReturning,
        }
    }
}

/// One decision on a taken place and how it ended: what the typed result
/// of an undo or a reconciliation carries for it, alongside the event in
/// the entry's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub journal_id: i64,
    /// The choice made; `None` when there was none to make — a conflict
    /// that appeared after a preview that did not show it.
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
/// done before it stopped, if any, and the decision on its taken place, if
/// it met one — for the command line and the web alike.
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
    /// quarantine path, read when the choice was offered: a change to it
    /// since is a different conflict. Such a unit is only ever kept, so
    /// this is part of the snapshot a decision is bound to, never a reason
    /// to move anything. `None` when the journal recorded evidence.
    pub seen: Option<Proof>,
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
                    proof: r.proof.clone().or_else(|| r.seen.clone()),
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

/// A choice made on a conflict seen earlier — in a preview — and carried out
/// later. It holds only while the conflict read then is the conflict read
/// now: the same files coming back, and the same objects, by their
/// evidence, bearing their places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewed {
    pub seen: Conflict,
    pub choice: Choice,
}

/// [`crate::undo`] bound to a preview (el-14vx0): `reviewed` is the
/// conflict the preview showed for this entry and the choice made on it,
/// `None` when the preview showed none. The conflict read now must be
/// exactly that one — or, without one, there must be none — and the
/// choice is then carried out; anything else is refused as changed since
/// the preview, nothing moved and the refusal written down. A reviewed
/// "keep" is carried out as such: written down and reported.
pub fn undo_reviewed(db: &Db, journal_id: i64, reviewed: Option<&Reviewed>) -> Result<Tally> {
    let Some(entry) = db.journal_entry(journal_id)? else {
        return crate::undo(db, journal_id);
    };
    let now = undo_conflict(db, &entry)?;
    match reviewed {
        Some(r) => match now {
            Some(now) if now == r.seen => resolve(db, &entry, now, r.choice),
            now => Err(changed(db, &entry, Some(r), now.as_ref())),
        },
        // None shown, and a conflict now: one that appeared since, and no
        // one has chosen anything about it.
        None => crate::undo_with(db, journal_id, &mut |now| {
            Err(changed(db, &entry, None, Some(now)))
        }),
    }
}

/// [`crate::reconcile_undo`] bound to a preview, as [`undo_reviewed`].
pub fn reconcile_reviewed(
    db: &Db,
    journal_id: i64,
    reviewed: Option<&Reviewed>,
) -> Result<recovery::Reconciled> {
    let Some(entry) = db.journal_entry(journal_id)? else {
        return recovery::reconcile_undo(db, journal_id);
    };
    let now = reconcile_conflict(db, &entry)?;
    match reviewed {
        Some(r) => match now {
            Some(now) if now == r.seen => {
                let items = recovery::reconcile(db, journal_id)?;
                let done = resolve(db, &entry, now, r.choice)?;
                Ok(recovery::Reconciled { items, done })
            }
            now => Err(changed(db, &entry, Some(r), now.as_ref())),
        },
        None => recovery::reconcile_undo_with(db, journal_id, &mut |now| {
            Err(changed(db, &entry, None, Some(now)))
        }),
    }
}

/// The refusal of a unit whose conflict is not the one the preview showed
/// (`r`, or none): written down with the choice made, the conflict seen and
/// the conflict now, typed as [`Outcome::ChangedSincePreview`]. Nothing is
/// moved.
fn changed(
    db: &Db,
    entry: &JournalEntry,
    r: Option<&Reviewed>,
    now: Option<&Conflict>,
) -> anyhow::Error {
    let then = match r {
        Some(r) => r.seen.describe(),
        None => pc_core::tr!("место было свободно", "the place was free").to_string(),
    };
    let now_words = match now {
        Some(c) => c.describe(),
        None => {
            let items = if entry.status == JournalStatus::Pending {
                recovery::reconcile(db, entry.id)
            } else {
                recovery::undo_preview(entry)
            };
            items
                .map(|items| recovery::listing(&items))
                .unwrap_or_else(|e| format!("{e:#}"))
        }
    };
    let why = pc_core::tf!(
        "запись {0}: с предпросмотра на исходном месте или в карантине что-то изменилось; \
         ничего не перенесено — обновите предпросмотр и выберите снова. Было: {1}. Сейчас: {2}",
        "entry {0}: something changed at the original place or in quarantine since the preview; \
         nothing was moved — refresh the preview and choose again. Then: {1}. Now: {2}",
        entry.id,
        then,
        now_words
    );
    let note = match (r, now) {
        (Some(r), _) => r.seen.note(r.choice),
        (None, Some(c)) => {
            let mut n = c.note(Choice::Keep);
            n.choice = String::new();
            n
        }
        (None, None) => ConflictNote {
            choice: String::new(),
            returning: Vec::new(),
            occupants: Vec::new(),
            returned_as: Vec::new(),
            aside_entry: None,
            outcome: None,
        },
    };
    noted(
        db,
        entry,
        r.map(|r| r.choice),
        Outcome::ChangedSincePreview,
        &why,
        note,
        anyhow!("{why}"),
    )
}

/// The conflict an undo of `entry` would meet now, if its only obstacle is
/// that its place is taken. Reads only.
pub fn undo_conflict(db: &Db, entry: &JournalEntry) -> Result<Option<Conflict>> {
    if !recovery::undo_offered(entry) {
        return Ok(None);
    }
    current(db, entry)
}

/// The conflict a reconciliation of the interrupted `entry` would meet now,
/// if its only obstacle is that the place is taken: the same choices as an
/// undo (el-14vx0 B3). Reads only.
pub fn reconcile_conflict(db: &Db, entry: &JournalEntry) -> Result<Option<Conflict>> {
    if entry.status != JournalStatus::Pending || entry.op.contains("purge") {
        return Ok(None);
    }
    current(db, entry)
}

/// The conflict of `entry` as read now, from the list its undo or its
/// reconciliation walks back.
fn current(db: &Db, entry: &JournalEntry) -> Result<Option<Conflict>> {
    let Ok(list) = list_for(entry) else {
        return Ok(None);
    };
    of(db, entry, &recovery::pairs_of(entry, list))
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
/// `entry` met. However it ends, the decision is in the entry's history
/// with its outcome and in the typed result: the [`Tally`] of a choice
/// carried out, the error ([`outcome_of`]) of any other.
pub(crate) fn resolve(db: &Db, entry: &JournalEntry, c: Conflict, choice: Choice) -> Result<Tally> {
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
            c.note(choice),
            anyhow!("{why}"),
        ));
    }
    if choice == Choice::Keep {
        let why = c.kept_words();
        let note = c.note(choice);
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
    // The choice may have taken a person minutes. Immediately before the
    // first move the whole conflict is read again — the unit coming back by
    // its evidence, the existing frame with every companion it has now —
    // and anything different from what the choice was made on refuses it,
    // nothing moved (el-14vx0 B1, B2).
    let (fresh, list) = match again(db, entry, &c) {
        Ok(x) => x,
        Err(why) => {
            return Err(noted(
                db,
                entry,
                Some(choice),
                Outcome::ChangedSincePreview,
                &why,
                c.note(choice),
                anyhow!("{why}"),
            ))
        }
    };
    let carried = match choice {
        Choice::RenameReturning => returning_renamed(db, &fresh, list, &c),
        _ => aside_then_back(db, &fresh, list, &c, choice),
    };
    match carried {
        Ok(mut done) => {
            done.decisions.push(Decision {
                journal_id: entry.id,
                choice: Some(choice),
                outcome: Outcome::of(choice),
            });
            Ok(done)
        }
        Err(e) if decided_in(&e).is_some() => Err(e),
        // Refused on the way — a move, the journal: written down as this
        // decision's outcome, with the words of what happened.
        Err(e) => {
            let why = format!("{e:#}");
            Err(noted(
                db,
                entry,
                Some(choice),
                Outcome::Refused,
                &why,
                c.note(choice),
                e,
            ))
        }
    }
}

/// `entry` and its list as read now, if its conflict is still exactly `c`;
/// otherwise what changed, in words.
fn again(
    db: &Db,
    entry: &JournalEntry,
    c: &Conflict,
) -> std::result::Result<(JournalEntry, Vec<Moved>), String> {
    let changed = |now: String| {
        pc_core::tf!(
            "запись {0}: с тех пор как был показан выбор, на исходном месте или в карантине \
             что-то изменилось; ничего не перенесено — обновите предпросмотр и выберите снова. \
             Было: {1}. Сейчас: {2}",
            "entry {0}: something changed at the original place or in quarantine since the \
             choice was shown; nothing was moved — refresh the preview and choose again. \
             Then: {1}. Now: {2}",
            c.journal_id,
            c.describe(),
            now
        )
    };
    let fresh = match db.journal_entry(entry.id) {
        Ok(Some(e)) if e.status == entry.status => e,
        Ok(Some(e)) => {
            return Err(changed(pc_core::tf!(
                "запись в состоянии «{0}»",
                "the entry is “{0}”",
                e.status.as_str()
            )))
        }
        Ok(None) => {
            return Err(changed(
                pc_core::tr!("записи больше нет", "the entry is gone").to_string(),
            ))
        }
        Err(e) => return Err(changed(format!("{e:#}"))),
    };
    let list = list_for(&fresh).map_err(|e| changed(format!("{e:#}")))?;
    let pairs = recovery::pairs_of(&fresh, list.clone());
    match of(db, &fresh, &pairs) {
        Ok(Some(now)) if now == *c => Ok((fresh, list)),
        Ok(Some(now)) => Err(changed(now.describe())),
        Ok(None) => {
            let items: Vec<Item> = pairs.iter().map(recovery::item_of).collect();
            Err(changed(recovery::listing(&items)))
        }
        Err(e) => Err(changed(format!("{e:#}"))),
    }
}

/// Every member of the unit coming back proven where it is held, by its
/// evidence; otherwise where everything is, in words.
fn returning_unproven(pairs: &[Pair]) -> Option<String> {
    let proven = pairs.iter().all(|p| {
        p.look.proof.is_some()
            && matches!(
                recovery::spot(&p.look.dst, p.look.proof.as_ref()),
                Spot::Proven
            )
    });
    if proven {
        return None;
    }
    let items: Vec<Item> = pairs.iter().map(recovery::item_of).collect();
    Some(recovery::listing(&items))
}

/// What bears the place of the existing unit after it was set aside: a file
/// at one of its paths again, or — when the existing frame was part of it —
/// a companion of that frame's name (el-14vx0 B1). Any such file is not
/// part of what was reviewed.
fn newcomers(c: &Conflict) -> Vec<String> {
    let mut there: Vec<String> = c
        .occupants
        .iter()
        .filter(|o| fs::symlink_metadata(&o.path).is_ok())
        .map(|o| o.path.clone())
        .collect();
    let home = &c.returning[0].home;
    if c.occupants.iter().any(|o| &o.path == home) {
        for side in crate::files::companions(Path::new(home)) {
            let side = side.to_string_lossy().into_owned();
            if !there.contains(&side) {
                there.push(side);
            }
        }
    }
    there
}

/// Choice 4: the unit comes back under the first free name, the existing
/// file is not touched.
fn returning_renamed(
    db: &Db,
    entry: &JournalEntry,
    list: Vec<Moved>,
    c: &Conflict,
) -> Result<Tally> {
    let id = entry.id;
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
                entry,
                Some(Choice::RenameReturning),
                Outcome::Refused,
                &why,
                c.note(Choice::RenameReturning),
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
        db.journal_event_conflict(id, phase_of(entry), "attempt", &text, &note)?;
        match recovery::move_back(&pairs) {
            Back::Done(arrived, came) => {
                note.outcome = Some(Outcome::RenamedReturning.as_str().to_string());
                return recovery::finish(
                    db,
                    entry,
                    &pairs,
                    arrived,
                    came,
                    Tally::default(),
                    Some(&note),
                );
            }
            Back::Failed(Unit::Refused(e)) if crate::is_taken(&e) => continue,
            Back::Failed(Unit::PutBack(pb))
                if pb.whole && !pb.told.folder_moved && crate::is_taken(&pb.error) =>
            {
                continue
            }
            Back::Failed(unit) => {
                return Err(recovery::unit_failed(db, id, phase_of(entry), &items, unit))
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
    Err(noted(
        db,
        entry,
        Some(Choice::RenameReturning),
        Outcome::Refused,
        &why,
        c.note(Choice::RenameReturning),
        anyhow!("{why}"),
    ))
}

/// Choices 2 and 3: the existing unit is set aside first — into quarantine,
/// or to a free name beside it — under an entry of its own; then the place
/// is free, and the undo is the ordinary one, by evidence.
fn aside_then_back(
    db: &Db,
    entry: &JournalEntry,
    list: Vec<Moved>,
    c: &Conflict,
    choice: Choice,
) -> Result<Tally> {
    let id = entry.id;
    let phase = phase_of(entry);
    // The whole unit coming back is proven by its evidence before the
    // existing one is touched: a unit that could not come back must not
    // have displaced anything (el-14vx0 B2).
    if let Some(where_) = returning_unproven(&recovery::pairs_of(entry, list.clone())) {
        let why = pc_core::tf!(
            "{0}: ничего не перенесено — существующий файл не тронут, потому что \
             возвращающийся кадр со спутниками не доказан в карантине: {1}",
            "{0}: nothing was moved — the existing file was not touched, because the frame \
             and companions coming back are not proven in quarantine: {1}",
            phase,
            where_
        );
        return Err(noted(
            db,
            entry,
            Some(choice),
            Outcome::Refused,
            &why,
            c.note(choice),
            anyhow!("{why}"),
        ));
    }
    let (aside_id, moved) = match set_aside(db, entry, c, choice) {
        Ok(x) => x,
        Err(e) => {
            let why = format!("{e:#}");
            return Err(noted(
                db,
                entry,
                Some(choice),
                Outcome::Refused,
                &why,
                c.note(choice),
                e,
            ));
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
    if let Err(e) = db.journal_event_conflict(id, phase, "set-aside", &text, &note) {
        return Err(stop_run(e.context(text), &done, Route::Restore, Vec::new()));
    }
    // Something new at the place since the existing unit left — a companion
    // of its frame's name above all — would end up beside the frame coming
    // back. It does not come back; what was set aside stays set aside under
    // its own undoable entry, and asking again decides afresh.
    let new = newcomers(c);
    if !new.is_empty() {
        let why = pc_core::tf!(
            "{0}: после того как существующий файл был отложен (запись {1}), на его месте \
             появилось новое: {2}; возвращающийся файл остаётся в карантине: {3}",
            "{0}: after the existing file was set aside (entry {1}), something new appeared \
             at its place: {2}; the file coming back stays in quarantine: {3}",
            phase,
            aside_id,
            new.join("; "),
            c.returning[0].held
        );
        let e = noted(
            db,
            entry,
            Some(choice),
            Outcome::Refused,
            &why,
            note,
            anyhow!("{why}"),
        );
        return Err(stop_run(e, &done, Route::Restore, Vec::new()));
    }
    let entry = db
        .journal_entry(id)?
        .with_context(|| pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", id))?;
    let pairs = recovery::pairs_of(&entry, list);
    match recovery::walk_back(db, id, phase, &pairs) {
        Ok((arrived, came)) => {
            note.outcome = Some(Outcome::of(choice).as_str().to_string());
            recovery::finish(db, &entry, &pairs, arrived, came, done, Some(&note))
        }
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
        // And whatever appears at their place while they move — a new
        // companion of the existing frame — sends them all back: a unit is
        // never split (el-14vx0 B1).
        let left = || -> Option<String> {
            let new = newcomers(c);
            (!new.is_empty()).then(|| {
                pc_core::tf!(
                    "пока существующий файл откладывался, на его месте появилось новое: {0}",
                    "while the existing file was being set aside, something new appeared at \
                     its place: {0}",
                    new.join("; ")
                )
            })
        };
        match move_unit_checked(&members, Way::Forward, None, &[], &left) {
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
