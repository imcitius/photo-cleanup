//! Folders the user has named as holding the archive's originals.
//!
//! An archive is not always one tree. On an array the photographs are spread
//! over `/mnt/disk1`, `/mnt/disk2`, `/mnt/disk3` — separate filesystems,
//! because a move has to stay on one spindle to be a rename — and the person
//! who owns them sees one folder structure laid across the three. Told "the
//! originals are in `D/разобрано/даня/театр`", they mean it about all of
//! them, and saying it three times is saying it wrong: a disk added to the
//! array next month would not be covered by any of the three.
//!
//! So a mark is one of two things, and both are needed:
//!
//! * `Absolute` — this folder on this disk, nothing else. What you reach for
//!   when one copy of a structure is the good one and the others are not.
//! * `EveryRoot` — this path *relative to a root*, under every root the
//!   archive is configured with. One row, one decision, and the array can
//!   grow underneath it.

use anyhow::Result;
use rusqlite::params;

use crate::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkScope {
    /// One folder, named in full.
    Absolute,
    /// A path relative to a root, meaning that folder under all of them.
    EveryRoot,
}

impl MarkScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absolute => "absolute",
            Self::EveryRoot => "every-root",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "every-root" => Self::EveryRoot,
            _ => Self::Absolute,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Mark {
    /// Absolute for `Absolute`, relative to a root for `EveryRoot`.
    pub path: String,
    pub scope: MarkScope,
}

/// Every mark, with the roots a relative one is read against.
///
/// Built once and asked many times: settling ten thousand groups asks this
/// about every file in the archive, and it must not go back to the database
/// to do it.
#[derive(Debug, Clone, Default)]
pub struct Marks {
    pub marks: Vec<Mark>,
    /// Longest first, so a root nested inside another wins — the answer to
    /// "which root is this file under" has to be the most specific one, or a
    /// relative path would carry the inner root's name in front of it.
    pub roots: Vec<String>,
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// The root this path lies under, and what is left of the path below it.
    ///
    /// A file under no configured root — indexed when the roots were set
    /// differently, and still in the archive — has no relative reading, and
    /// only an absolute mark can speak about it.
    pub fn split<'a>(&'a self, path: &'a str) -> Option<(&'a str, &'a str)> {
        let root = self.roots.iter().find(|r| pc_core::under(path, r))?;
        let rest = pc_core::trim_leading_separators(&path[root.len()..]);
        Some((root.as_str(), rest))
    }

    /// The mark covering this path, if any.
    ///
    /// When several do, the most specific wins — the one whose own path is
    /// longest. It only decides which mark is *reported*; whether the path is
    /// covered at all is the same answer either way.
    pub fn covering(&self, path: &str) -> Option<&Mark> {
        let relative = self.split(path).map(|(_, rest)| rest);
        self.marks
            .iter()
            .filter(|m| match m.scope {
                MarkScope::Absolute => pc_core::under(path, &m.path),
                MarkScope::EveryRoot => relative.is_some_and(|r| pc_core::under(r, &m.path)),
            })
            .max_by_key(|m| m.path.len())
    }

    /// The mark covering a node of the *merged* tree, whose path is already
    /// relative to a root.
    ///
    /// Only a relative mark can cover such a node: an absolute one speaks
    /// about one disk's copy of it, and the merged node is all of them. The
    /// per-disk rows under the node answer that question for themselves.
    pub fn covering_relative(&self, rel: &str) -> Option<&Mark> {
        self.marks
            .iter()
            .filter(|m| m.scope == MarkScope::EveryRoot && pc_core::under(rel, &m.path))
            .max_by_key(|m| m.path.len())
    }

    /// True when this folder is marked in its own right, rather than covered
    /// by one further up.
    pub fn marked(&self, path: &str, scope: MarkScope) -> bool {
        self.marks
            .iter()
            .any(|m| m.scope == scope && m.path == path)
    }
}

impl Db {
    /// The folders the archive is configured to read.
    ///
    /// These are what a relative mark is relative to. Longest first, so the
    /// most specific root wins for a file that lies under two of them.
    pub fn archive_roots(&self) -> Result<Vec<String>> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = 'roots'", [], |r| {
                r.get(0)
            })
            .ok();
        let mut roots: Vec<String> = raw
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|r| pc_core::trim_trailing_separators(&r).to_string())
            .filter(|r| !r.is_empty())
            .collect();
        roots.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        roots.dedup();
        Ok(roots)
    }

    pub fn original_marks(&self) -> Result<Marks> {
        let mut st = self
            .conn
            .prepare("SELECT path, scope FROM original_folders ORDER BY scope, path")?;
        let marks = st
            .query_map([], |r| {
                Ok(Mark {
                    path: r.get(0)?,
                    scope: MarkScope::parse(&r.get::<_, String>(1)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Marks {
            marks,
            roots: self.archive_roots()?,
        })
    }

    /// Say that this folder — and everything beneath it — holds originals.
    ///
    /// A folder already covered by a mark of the same kind further up is not
    /// written down again: the list would then carry two rows saying the same
    /// thing, and taking the inner one back would look like it had changed
    /// something. Marks of that kind *below* this one are absorbed for the
    /// same reason. The two kinds do not absorb each other — naming one disk
    /// and naming the same path everywhere are different sentences, and a
    /// person may well want both.
    ///
    /// Runs inside the caller's transaction: naming a folder and applying
    /// what follows from it is one decision, and half of it is worse than
    /// neither half.
    pub fn mark_original(&self, path: &str, scope: MarkScope) -> Result<()> {
        let existing = self.original_marks()?;
        let same: Vec<&Mark> = existing.marks.iter().filter(|m| m.scope == scope).collect();
        if same.iter().any(|m| pc_core::under(path, &m.path)) {
            return Ok(());
        }
        for inner in same.iter().filter(|m| pc_core::under(&m.path, path)) {
            self.conn.execute(
                "DELETE FROM original_folders WHERE path = ?1 AND scope = ?2",
                params![inner.path, scope.as_str()],
            )?;
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO original_folders(path, marked_at, scope) VALUES(?1, ?2, ?3)",
            params![path, crate::pc_core_now(), scope.as_str()],
        )?;
        Ok(())
    }

    /// Take the mark back, and with it every kept file it chose.
    ///
    /// Only the ones it chose: a file the user pressed keeps its mark. What
    /// the rule decided and nothing else now says is undone, and the groups
    /// left without an answer fall back to their best version, which is where
    /// they were before the folder was ever named.
    ///
    /// Runs inside the caller's transaction, for the same reason as the mark.
    pub fn unmark_original(&self, path: &str, scope: MarkScope) -> Result<usize> {
        let removed = self.conn.execute(
            "DELETE FROM original_folders WHERE path = ?1 AND scope = ?2",
            params![path, scope.as_str()],
        )?;
        let still = self.original_marks()?;
        let freed: Vec<(i64, i64)> = {
            let mut st = self.conn.prepare(
                "SELECT k.file_id, m.family_id, f.path
                   FROM manual_keepers k
                   JOIN files f          ON f.id = k.file_id
                   JOIN family_members m ON m.file_id = k.file_id
                  WHERE k.source = 'folder'",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .filter(|(_, _, p)| still.covering(p).is_none())
                .map(|(file, family, _)| (file, family))
                .collect()
        };
        for (file, family) in &freed {
            self.conn
                .execute("DELETE FROM manual_keepers WHERE file_id = ?1", [file])?;
            self.conn.execute(
                "UPDATE families SET keeper_file =
                   (SELECT file_id FROM family_members
                     WHERE family_id = ?1 ORDER BY COALESCE(quality, 0) DESC, file_id LIMIT 1)
                  WHERE id = ?1",
                [family],
            )?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(roots: &[&str], list: &[(&str, MarkScope)]) -> Marks {
        let mut roots: Vec<String> = roots.iter().map(|r| r.to_string()).collect();
        roots.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        Marks {
            roots,
            marks: list
                .iter()
                .map(|(p, s)| Mark {
                    path: p.to_string(),
                    scope: *s,
                })
                .collect(),
        }
    }

    #[test]
    fn one_relative_mark_speaks_for_every_disk_of_an_array() {
        let m = marks(
            &["/mnt/disk1", "/mnt/disk2", "/mnt/disk3"],
            &[("D/разобрано/даня/театр", MarkScope::EveryRoot)],
        );
        for disk in ["disk1", "disk2", "disk3"] {
            let path = format!("/mnt/{disk}/D/разобрано/даня/театр/IMG_1.JPG");
            assert!(m.covering(&path).is_some(), "{path}");
        }
        assert!(m
            .covering("/mnt/disk2/D/разобрано/даня/театр/сканы/IMG_2.JPG")
            .is_some());
        assert!(m
            .covering("/mnt/disk1/D/разобрано/даня/цирк/IMG_3.JPG")
            .is_none());
    }

    #[test]
    fn a_disk_outside_the_roots_is_not_covered_by_a_relative_mark() {
        // Its path has no relative reading, so nothing relative can claim it.
        let m = marks(&["/mnt/disk1"], &[("D/разобрано", MarkScope::EveryRoot)]);
        assert!(m.covering("/mnt/disk9/D/разобрано/a.jpg").is_none());
        assert!(m.covering("/mnt/disk1/D/разобрано/a.jpg").is_some());
    }

    #[test]
    fn an_absolute_mark_still_means_one_disk_and_only_that_one() {
        let m = marks(
            &["/mnt/disk1", "/mnt/disk2"],
            &[("/mnt/disk1/D/разобрано", MarkScope::Absolute)],
        );
        assert!(m.covering("/mnt/disk1/D/разобрано/a.jpg").is_some());
        assert!(m.covering("/mnt/disk2/D/разобрано/a.jpg").is_none());
    }

    #[test]
    fn the_most_specific_root_decides_what_relative_means() {
        let m = marks(
            &["/mnt/disk1", "/mnt/disk1/D/архив"],
            &[("2014", MarkScope::EveryRoot)],
        );
        // Read against the inner root the rest is `2014`; against the outer
        // one it would be `D/архив/2014` and nothing would match.
        assert!(m.covering("/mnt/disk1/D/архив/2014/a.jpg").is_some());
        assert!(m.covering("/mnt/disk1/D/другое/2014/a.jpg").is_none());
    }

    #[test]
    fn a_name_that_merely_starts_the_same_is_a_different_folder() {
        let m = marks(&["/mnt/disk1"], &[("D/2014", MarkScope::EveryRoot)]);
        assert!(m.covering("/mnt/disk1/D/2014-old/a.jpg").is_none());
        assert!(m.covering("/mnt/disk1/D/2014/a.jpg").is_some());
    }
}
