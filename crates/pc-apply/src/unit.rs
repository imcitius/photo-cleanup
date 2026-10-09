//! A photograph and its companions move as one unit, or not at all (user
//! decision (c) of 2026-10-05, after el-4pk1q B1-R3..B4-R3).
//!
//! A frame used to move first and its sidecars after it, each on its own;
//! a sidecar that could not follow was noted, and the frame stayed done.
//! Every later step then had to know which half went where: the undo, the
//! reconciliation, the record, the status. Three review rounds found a half
//! that one of them had missed — an edit left in quarantine behind an entry
//! closed as undone, a refused sidecar recorded where it no longer was, a
//! reconciliation that brought the frame back and lost track of it.
//!
//! So there are no halves. Every operation — apply, organize, a litter
//! sweep, an undo, a reconciliation — moves a [`Member`] list as one:
//!
//! 1. Before the first rename every member is bound and held
//!    ([`Held::bind`]) and proven to be the object the plan or the journal
//!    describes, and every destination name after the first is free. Any
//!    doubt refuses the whole unit; nothing moves.
//! 2. The members move in order. If any move is refused, or right before
//!    the record any member is no longer proven where it went (or its
//!    origin folder moved), every member that moved is put back, newest
//!    first, into the folder held for it, never over anything
//!    ([`Arrived::give_back`]). The unit is refused, not done.
//! 3. If putting back cannot complete — an object of the unit stays in the
//!    operation's keeping — the caller stops its run, leaves the row open,
//!    and records and shows where every member is, proven after the last
//!    rename. Nothing is ever deleted.
//!
//! The price is stated in DESIGN §11.2: when another program interferes,
//! frames are refused more often. That is meant — leave extra, never lose.

use anyhow::anyhow;
use pc_core::proof::Proof;
use pc_core::whereabouts::Whereabouts;
use pc_db::{Db, Moved};
use std::path::Path;

use crate::bound::{rename_bound, Arrived, Held, Object, Role};
use crate::located::{objects_of, Side, Told, Way};
use crate::{FolderMoved, Placed, Route};

/// One member of a unit: the item as recorded, and which way it goes now.
pub(crate) struct Member<'a> {
    pub(crate) rec: &'a Moved,
    pub(crate) from: &'a Path,
    pub(crate) to: &'a Path,
    pub(crate) expect: Option<&'a Proof>,
}

impl<'a> Member<'a> {
    /// As planned: from `src` to `dst`.
    pub(crate) fn forward(rec: &'a Moved) -> Self {
        Member {
            rec,
            from: Path::new(&rec.src),
            to: Path::new(&rec.dst),
            expect: rec.proof.as_ref(),
        }
    }

    /// Walking `rec` back: from where it is looked for (`look.dst`, which
    /// may be a place the journal later proved) to its recorded `src`.
    pub(crate) fn back(rec: &'a Moved, look: &'a Moved) -> Self {
        Member {
            rec,
            from: Path::new(&look.dst),
            to: Path::new(&look.src),
            expect: look.proof.as_ref(),
        }
    }
}

/// How a unit's move ended.
pub(crate) enum Unit {
    /// Every member moved and was proven where it went, its origin folder
    /// where its path says, right before the record. Held until recorded.
    Moved(Vec<Arrived>),
    /// Refused before the first rename: nothing moved.
    Refused(anyhow::Error),
    /// Something moved and the unit was put back, whole or not.
    PutBack(PutBack),
}

/// A unit that moved in part and was put back.
pub(crate) struct PutBack {
    /// What refused first.
    pub(crate) error: anyhow::Error,
    /// Every object touched — and, when the unit could not be put back
    /// whole, every member — with its place proven after the last rename.
    pub(crate) told: Told,
    /// An object of the unit's own is still in the operation's keeping:
    /// putting back did not complete, the row stays open.
    pub(crate) kept: bool,
    /// Every object touched is proven back at its origin.
    pub(crate) whole: bool,
}

impl PutBack {
    /// In words: why, and where everything is.
    pub(crate) fn why(&self) -> String {
        let how = if self.whole {
            pc_core::tr!(
                "кадр со спутниками возвращён целиком, ничего не перенесено",
                "the frame and its companions were put back whole, nothing moved"
            )
        } else if self.kept {
            pc_core::tr!(
                "кадр со спутниками не удалось вернуть целиком: запись остаётся открытой, \
                 ничего не удалено",
                "the frame and its companions could not be put back whole: the entry stays \
                 open, nothing was removed"
            )
        } else {
            pc_core::tr!(
                "кадр со спутниками возвращён, но не всё доказано на своём месте",
                "the frame and its companions were put back, but not everything is proven at \
                 its place"
            )
        };
        let words = self.told.words();
        if words.is_empty() {
            format!("{}; {how}", self.error)
        } else {
            format!("{}; {how}: {words}", self.error)
        }
    }

    /// The run cannot go on after this unit.
    pub(crate) fn stops(&self) -> bool {
        !self.whole || self.told.folder_moved || crate::is_no_exclusive_rename(&self.error)
    }

    /// The error a run stops with, typed: the volume's refusal stays one.
    fn cause(self, why: String) -> anyhow::Error {
        if crate::is_no_exclusive_rename(&self.error) {
            self.error
        } else if self.told.folder_moved && !self.kept {
            FolderMoved { reason: why }.into()
        } else {
            anyhow!("{why}")
        }
    }
}

/// Why a unit refuses: one member could not be held as checked.
fn not_held(m: &Member<'_>, e: anyhow::Error) -> anyhow::Error {
    anyhow!(pc_core::tf!(
        "{0} — не удержан как проверенный ({1}); кадр со спутниками не переносится, ничего не \
         перенесено",
        "{0} — could not be held as checked ({1}); the frame and its companions are not moved, \
         nothing moved",
        m.from.display(),
        format!("{e:#}")
    ))
}

/// What, right before the record, is no longer proven about a member that
/// moved: its place, or the folder it came from.
fn doubt(a: &Arrived, m: &Member<'_>) -> Option<String> {
    let at = a.locate();
    if !at.is_verified_at(a.dst()) {
        return Some(pc_core::tf!(
            "{0} — после переноса не доказано, что он там, куда перенесён ({1})",
            "{0} — after the move it is not proven where it was moved to ({1})",
            m.from.display(),
            at.shown()
        ));
    }
    if !a.origin_holds() {
        return Some(match a.origin_now() {
            Some(p) => pc_core::tf!(
                "{0} — папку, откуда он перенесён, переместили во время операции; система \
                 называет её теперь {1} (не проверено)",
                "{0} — the folder it was moved from was moved during the operation; the system \
                 now names it {1} (unverified)",
                m.from.display(),
                p.display()
            ),
            None => pc_core::tf!(
                "{0} — папку, откуда он перенесён, переместили во время операции",
                "{0} — the folder it was moved from was moved during the operation",
                m.from.display()
            ),
        });
    }
    None
}

/// Move `members` as one unit. `first`, when given, is member 0 already held
/// (and checked against `keepers`, which are verified once more right before
/// its rename). See the module comment.
pub(crate) fn move_unit(
    members: &[Member<'_>],
    way: Way,
    first: Option<Held>,
    keepers: &[&Held],
) -> Unit {
    move_unit_checked(members, way, first, keepers, &|| None)
}

/// [`move_unit`], asking `left` too, after the last rename and right before
/// the record, whether what the unit left behind at its origin is as it
/// should be (el-14vx0 B1: a companion of the existing frame that appeared
/// while it moved would be split from it). Any answer sends the whole unit
/// back, as any other doubt does.
pub(crate) fn move_unit_checked(
    members: &[Member<'_>],
    way: Way,
    first: Option<Held>,
    keepers: &[&Held],
    left: &dyn Fn() -> Option<String>,
) -> Unit {
    // 1. Every member bound, held and proven, every later name free.
    let mut first = first;
    let mut held = Vec::with_capacity(members.len());
    for (i, m) in members.iter().enumerate() {
        let h = match first.take().filter(|_| i == 0) {
            Some(h) => h,
            None => match Held::bind(m.from, m.expect) {
                Ok(h) => h,
                Err(e) => return Unit::Refused(not_held(m, e)),
            },
        };
        held.push(h);
    }
    for m in members.iter().skip(1) {
        if std::fs::symlink_metadata(m.to).is_ok() {
            return Unit::Refused(
                crate::Taken {
                    path: m.to.to_path_buf(),
                    unit: true,
                }
                .into(),
            );
        }
    }

    // 2. In order; the first refusal ends it.
    let mut arrived: Vec<Arrived> = Vec::new();
    let mut failed = None;
    for (i, (m, h)) in members.iter().zip(&held).enumerate() {
        let ks: &[&Held] = if i == 0 { keepers } else { &[] };
        match rename_bound(h, m.to, ks) {
            Ok(a) => arrived.push(a),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    let k = arrived.len();
    let mut asked = false;
    let mut doubted = false;
    let error = match failed {
        None => {
            // Asked again after the last rename, right before the record
            // (el-lvtmk R4): any doubt about any member sends the unit back.
            crate::before_record(members[0].rec);
            asked = true;
            let doubts: Vec<String> = arrived
                .iter()
                .zip(members)
                .filter_map(|(a, m)| doubt(a, m))
                .collect();
            if doubts.is_empty() {
                // Not a doubt about a member or its folder: the unit is put
                // back whole and refused, the run goes on.
                match left() {
                    None => return Unit::Moved(arrived),
                    Some(why) => anyhow!("{why}"),
                }
            } else {
                doubted = true;
                anyhow!("{}", doubts.join("; "))
            }
        }
        Some(e) if k == 0 && objects_of(&e).is_empty() => return Unit::Refused(e),
        Some(e) => e,
    };

    // 3. Put back what moved, newest first.
    let mut returned = vec![false; k];
    let mut notes = Vec::new();
    for i in (0..k).rev() {
        match arrived[i].give_back() {
            Ok(()) => returned[i] = true,
            Err(w) => notes.push(pc_core::tf!(
                "{0} — не возвращён: {1}",
                "{0} — not put back: {1}",
                members[i].from.display(),
                w
            )),
        }
    }
    if !asked {
        crate::before_record(members[0].rec);
    }

    // 4. Where everything is, proven now, after the last rename.
    struct Seen {
        member: usize,
        touched: bool,
        role: Role,
        at: Whereabouts,
        side: Side,
        proof: Option<Proof>,
    }
    let mut seen = Vec::new();
    for i in 0..members.len() {
        if i < k {
            let a = &arrived[i];
            seen.push(Seen {
                member: i,
                touched: true,
                role: Role::Checked,
                at: a.where_now(returned[i]),
                side: if returned[i] { Side::From } else { Side::To },
                proof: Some(a.proof().clone()),
            });
            continue;
        }
        let objects: &[Object] = if i == k { objects_of(&error) } else { &[] };
        if objects.is_empty() {
            seen.push(Seen {
                member: i,
                touched: false,
                role: Role::Checked,
                at: held[i].locate(),
                side: Side::From,
                proof: Some(held[i].proof().clone()),
            });
        }
        for o in objects {
            seen.push(Seen {
                member: i,
                touched: true,
                role: o.role.clone(),
                at: o.now(),
                side: o.side,
                proof: o.proof.clone(),
            });
        }
    }
    let mut kept = false;
    let mut whole = true;
    for s in &seen {
        // Only what this unit moved can be in its keeping; a member it never
        // reached, or an object another program moved, is said, not judged.
        if !s.touched || s.side == Side::Elsewhere {
            continue;
        }
        if !(s.side == Side::From && s.at.is_verified_at(members[s.member].from)) {
            whole = false;
        }
        if s.side == Side::To && s.role != Role::Stranger {
            kept = true;
        }
    }
    let mut told = Told {
        folder_moved: doubted || crate::located::folder_moved(&error),
        notes,
        ..Told::default()
    };
    for s in seen {
        // Put back whole: what was never touched says nothing.
        if whole && !s.touched {
            continue;
        }
        let o = Object {
            role: s.role,
            at: s.at.clone(),
            side: s.side,
            proof: s.proof,
            kept: None,
        };
        told.object(members[s.member].rec, way, &o, s.at);
    }
    // Held until everything above was said.
    drop(held);
    Unit::PutBack(PutBack {
        error,
        told,
        kept,
        whole,
    })
}

/// A forward unit that did not stay moved, as a refusal the run can go on
/// after, with the words and places to show.
pub(crate) struct Refusal {
    pub(crate) why: String,
    pub(crate) placed: Vec<Placed>,
    pub(crate) stop: Option<FolderMoved>,
}

/// Close the row `jid` of a forward unit that did not stay moved: refused
/// (`failed`, with every place it proved), or — when an object of the unit
/// stays in the operation's keeping — left open (`pending`) with every
/// member's place appended, and the run stopped. `Err` stops the run.
pub(crate) fn close_forward(
    db: &Db,
    jid: i64,
    phase: &str,
    route: Route,
    unit: Unit,
) -> std::result::Result<Refusal, anyhow::Error> {
    let pb = match unit {
        Unit::Moved(_) => unreachable!("a moved unit is recorded by its operation"),
        Unit::Refused(e) => {
            let shown = e.to_string();
            if let Err(pe) = crate::close_refused(db, jid, phase, &shown, &Told::default()) {
                return Err(crate::outcome::left_pending(pe.context(shown), jid, route));
            }
            if crate::is_no_exclusive_rename(&e) {
                return Err(e);
            }
            return Ok(Refusal {
                why: shown,
                placed: Vec::new(),
                stop: None,
            });
        }
        Unit::PutBack(pb) => pb,
    };
    let why = pb.why();
    if pb.kept {
        let recorded = db.journal_event_located(jid, phase, "kept", &why, &pb.told.located);
        let placed = pb.told.placed.clone();
        let mut e = pb.cause(why);
        if let Err(pe) = recorded {
            e = e.context(pc_core::tf!(
                "журнал не принял запись о местах ({0})",
                "the journal did not take the record of the places ({0})",
                format!("{pe:#}")
            ));
        }
        let e = crate::outcome::with_placed(e, placed, route);
        return Err(crate::outcome::left_pending(e, jid, route));
    }
    if let Err(pe) = crate::close_refused(db, jid, phase, &why, &pb.told) {
        let e = crate::outcome::with_placed(pe.context(why), pb.told.placed, route);
        return Err(crate::outcome::left_pending(e, jid, route));
    }
    if pb.whole && !pb.told.folder_moved && !crate::is_no_exclusive_rename(&pb.error) {
        return Ok(Refusal {
            why,
            placed: pb.told.placed,
            stop: None,
        });
    }
    if pb.told.folder_moved && !crate::is_no_exclusive_rename(&pb.error) {
        return Ok(Refusal {
            stop: Some(FolderMoved {
                reason: why.clone(),
            }),
            why,
            placed: pb.told.placed,
        });
    }
    let placed = pb.told.placed.clone();
    debug_assert!(pb.stops());
    Err(crate::outcome::with_placed(pb.cause(why), placed, route))
}
