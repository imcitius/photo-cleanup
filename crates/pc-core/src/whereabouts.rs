//! Where an object is, said only as far as it was proven (el-3wizg,
//! diagnosis el-lvtmk §3.1).
//!
//! Twice a move told the user that a photograph was "put straight back" at
//! a path that, by then, no longer led to the folder it had been put back
//! into (el-4z6z9 B1, el-57qpk B1-R2). The path was the one the move had
//! been *given*; nothing showed that it still named the object. Every place
//! the tool now states or records is one of these three, and only
//! [`Whereabouts::Verified`] says "it is at".
//!
//! The proof is taken after the last rename of the object in the operation
//! and right before the statement is shown or journaled; see
//! `anchored::locate` on Unix. A path that could not be proven is still
//! said — the user needs somewhere to look — but always as unverified.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "certainty", rename_all = "lowercase", deny_unknown_fields)]
pub enum Whereabouts {
    /// At this path, proven at the moment of saying so: the object's folder
    /// is the folder this path leads to, and the name in it bears the
    /// object.
    Verified { at: PathBuf },
    /// Its place could not be proven. `last_known` is where it was seen
    /// last, unverified, if anything was seen at all.
    Uncertain {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_known: Option<PathBuf>,
        why: String,
    },
    /// The object held open has no name any more: another program removed
    /// it. Its bytes live only as long as something holds it open.
    Unlinked,
}

impl Whereabouts {
    pub fn verified(at: impl Into<PathBuf>) -> Self {
        Whereabouts::Verified { at: at.into() }
    }

    pub fn uncertain(last_known: Option<PathBuf>, why: impl Into<String>) -> Self {
        Whereabouts::Uncertain {
            last_known,
            why: why.into(),
        }
    }

    /// The proven path, if there is one.
    pub fn at(&self) -> Option<&std::path::Path> {
        match self {
            Whereabouts::Verified { at } => Some(at),
            _ => None,
        }
    }

    /// Where to look: the proven path, or the last one seen.
    pub fn hint(&self) -> Option<&std::path::Path> {
        match self {
            Whereabouts::Verified { at } => Some(at),
            Whereabouts::Uncertain { last_known, .. } => last_known.as_deref(),
            Whereabouts::Unlinked => None,
        }
    }

    pub fn is_verified_at(&self, path: &std::path::Path) -> bool {
        self.at() == Some(path)
    }

    /// In words: "verified at …", or why not and where it was last seen.
    /// Never "it is at" for anything that was not proven.
    pub fn shown(&self) -> String {
        match self {
            Whereabouts::Verified { at } => {
                crate::tf!("проверено: лежит в {0}", "verified at {0}", at.display())
            }
            Whereabouts::Uncertain {
                last_known: Some(p),
                why,
            } => crate::tf!(
                "место не подтверждено ({0}); последнее известное, не проверено: {1}",
                "its place is not verified ({0}); last known, unverified: {1}",
                why,
                p.display()
            ),
            Whereabouts::Uncertain {
                last_known: None,
                why,
            } => crate::tf!(
                "где он теперь, неизвестно ({0})",
                "where it is now is unknown ({0})",
                why
            ),
            Whereabouts::Unlinked => crate::tr!(
                "другая программа удалила его: ни одно имя на него больше не ведёт",
                "another program removed it: no name leads to it any more"
            )
            .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_proven_place_is_said_to_be_where_it_is() {
        let v = Whereabouts::verified("/a/b");
        assert!(v.is_verified_at(std::path::Path::new("/a/b")));
        let u = Whereabouts::uncertain(Some("/a/b".into()), "moved");
        assert_eq!(u.at(), None);
        assert_eq!(u.hint(), Some(std::path::Path::new("/a/b")));
        assert!(Whereabouts::Unlinked.hint().is_none());
        for w in [v, u, Whereabouts::Unlinked] {
            let json = serde_json::to_string(&w).unwrap();
            assert_eq!(serde_json::from_str::<Whereabouts>(&json).unwrap(), w);
        }
    }
}
