//! Matching files on disk to the frames a Lightroom catalog curated.
//!
//! The paths will not agree. A catalog records what the machine that wrote
//! it saw, and that is rarely what we walk: on Unraid a catalog says
//! `/mnt/user/data/media/foto/...` — the FUSE share — while the scan reads
//! `/mnt/disk3/data/media/foto/...` directly, because the share layer hides
//! which spindle a file is on. On macOS `/var` resolves to `/private/var`.
//! Comparing full paths therefore silently protects nothing.
//!
//! So the match falls back to the tail of the path, which is identical
//! across every one of those cases. A wrong match here over-protects, which
//! is the harmless direction; a missed match deletes something curated.

use std::collections::HashMap;

/// How many trailing components must agree for a fallback match. Enough to
/// survive a Sony body reusing `DSC01234` across years and folders.
const TAIL: usize = 3;

fn tail_key(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let from = parts.len().saturating_sub(TAIL);
    parts[from..].join("/").to_lowercase()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Curated {
    pub rating: Option<i64>,
    /// True when the paths agreed outright rather than by their tails.
    pub exact: bool,
}

#[derive(Debug, Default)]
pub struct CurationIndex {
    by_path: HashMap<String, Option<i64>>,
    by_tail: HashMap<String, Option<i64>>,
    /// Tails that more than one catalogued file shares; too ambiguous to use.
    ambiguous: HashMap<String, bool>,
}

impl CurationIndex {
    pub fn build(entries: impl IntoIterator<Item = (String, Option<i64>)>) -> Self {
        let mut idx = Self::default();
        for (path, rating) in entries {
            let tail = tail_key(&path);
            if let Some(prev) = idx.by_tail.get(&tail) {
                // Two different catalogued files with the same tail: keep the
                // better rating and remember that the key is not unique.
                let best = match (prev, &rating) {
                    (Some(a), Some(b)) => Some(*a.max(b)),
                    (Some(a), None) => Some(*a),
                    (None, b) => *b,
                };
                idx.by_tail.insert(tail.clone(), best);
                idx.ambiguous.insert(tail, true);
            } else {
                idx.by_tail.insert(tail, rating);
            }
            idx.by_path.insert(path.to_lowercase(), rating);
        }
        idx
    }

    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    pub fn lookup(&self, path: &str) -> Option<Curated> {
        if let Some(rating) = self.by_path.get(&path.to_lowercase()) {
            return Some(Curated {
                rating: *rating,
                exact: true,
            });
        }
        // An ambiguous tail still protects: over-protecting costs the user a
        // decision, under-protecting costs them a photograph.
        self.by_tail.get(&tail_key(path)).map(|rating| Curated {
            rating: *rating,
            exact: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> CurationIndex {
        CurationIndex::build([
            (
                "/mnt/user/data/media/foto/2019/DSC01234.ARW".to_string(),
                Some(5),
            ),
            ("/var/folders/x/foto/Dump/DSC05555.JPG".to_string(), Some(3)),
        ])
    }

    #[test]
    fn an_exact_path_matches() {
        let c = index()
            .lookup("/mnt/user/data/media/foto/2019/DSC01234.ARW")
            .unwrap();
        assert!(c.exact);
        assert_eq!(c.rating, Some(5));
    }

    #[test]
    fn the_unraid_share_and_the_disk_behind_it_are_the_same_file() {
        // This is the case that silently disabled the protection: the catalog
        // says /mnt/user, the scan reads /mnt/disk3.
        let c = index()
            .lookup("/mnt/disk3/data/media/foto/2019/DSC01234.ARW")
            .unwrap();
        assert!(!c.exact);
        assert_eq!(c.rating, Some(5));
    }

    #[test]
    fn a_symlinked_prefix_still_matches() {
        let c = index()
            .lookup("/private/var/folders/x/foto/Dump/DSC05555.JPG")
            .unwrap();
        assert_eq!(c.rating, Some(3));
    }

    #[test]
    fn an_unrelated_file_does_not_match() {
        assert!(index()
            .lookup("/mnt/disk1/other/2019/DSC09999.ARW")
            .is_none());
    }

    #[test]
    fn a_reused_sony_number_in_another_folder_does_not_match() {
        // DSC01234 exists many times over in an archive spanning years; the
        // tail carries enough context to tell them apart.
        assert!(index()
            .lookup("/mnt/disk1/data/media/foto/2023/DSC01234.ARW")
            .is_none());
    }

    #[test]
    fn an_ambiguous_tail_keeps_the_stronger_rating() {
        let idx = CurationIndex::build([
            ("/a/foto/2019/DSC1.JPG".to_string(), Some(2)),
            ("/b/foto/2019/DSC1.JPG".to_string(), Some(5)),
        ]);
        assert_eq!(idx.lookup("/c/foto/2019/DSC1.JPG").unwrap().rating, Some(5));
    }
}
