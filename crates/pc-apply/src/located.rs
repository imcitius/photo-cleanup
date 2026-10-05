//! From a move's typed outcome to what is told and what is recorded
//! (el-3wizg, diagnosis el-lvtmk R1/R4/R5).
//!
//! A rename knows only its own two sides; which of them is "in quarantine"
//! depends on the operation: forward, the destination is; walking back, the
//! source is. Here that is settled once, and every object becomes both a
//! [`Placed`] for the person and a [`pc_db::Located`] for the journal — the
//! same object, the same proven place.

pub(crate) use crate::bound::Side;
use crate::bound::{Object, Unmoved};
use crate::outcome::Placed;
use pc_core::whereabouts::Whereabouts;
use pc_db::{Located, Moved};

/// Which way an operation's renames go relative to its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Way {
    /// From `src` to `dst`: apply, organize, a litter sweep, adoption.
    Forward,
    /// From `dst` back to `src`: undo, reconciliation.
    Back,
}

impl Way {
    fn held(self, side: Side) -> bool {
        matches!(
            (self, side),
            (Way::Forward, Side::To) | (Way::Back, Side::From)
        )
    }
}

/// What a unit that did not stay moved says about its objects, typed: the
/// same values for the person ([`Placed`]) and the journal ([`Located`]).
#[derive(Debug, Default)]
pub(crate) struct Told {
    pub(crate) placed: Vec<Placed>,
    pub(crate) located: Vec<Located>,
    pub(crate) folder_moved: bool,
    /// Words about the folders of the move, not about an object's place.
    pub(crate) notes: Vec<String>,
}

impl Told {
    /// Everything in words, for a journal note or a stop.
    pub(crate) fn words(&self) -> String {
        self.placed
            .iter()
            .map(|p| format!("{} — {}", p.recorded, p.shown()))
            .chain(self.notes.iter().cloned())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Object `o` of item `m`, at `at` — proven right before this is said.
    pub(crate) fn object(&mut self, m: &Moved, way: Way, o: &Object, at: Whereabouts) {
        let held = way.held(o.side);
        self.placed.push(Placed {
            recorded: m.src.clone(),
            role: o.role.clone(),
            held,
            at: at.clone(),
        });
        self.located.push(Located {
            src: m.src.clone(),
            dst: m.dst.clone(),
            role: o.role.as_str().to_string(),
            held,
            at,
            proof: o.proof.clone(),
        });
    }
}

/// The objects a refused move of a unit's member touched, as typed.
pub(crate) fn objects_of(e: &anyhow::Error) -> &[Object] {
    e.downcast_ref::<Unmoved>()
        .map(|u| u.objects.as_slice())
        .unwrap_or_default()
}

/// Whether the refusal says a folder of the move was moved.
pub(crate) fn folder_moved(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unmoved>().is_some_and(|u| u.folder_moved)
}
