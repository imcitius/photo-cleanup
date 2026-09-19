//! Turning dates and events into a concrete list of renames.
//!
//! Nothing here touches the filesystem. The plan is computed in full, shown
//! to the user, and only then applied — and every file the plan will not
//! move is listed with the reason, because a reorganisation that quietly
//! leaves a third of the archive behind is worse than one that refuses.

use anyhow::{Context, Result};
use pc_db::{Db, OrganizeRow};
use pc_family::curation::CurationIndex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::date::{self, Dated, Precision, Source};
use crate::events;

/// Where files with no usable date land, under their year when there is one.
const UNDATED_DIR: &str = "без-точной-даты";
/// Files with no date at all, not even a year.
const NO_DATE_DIR: &str = "без-даты";

#[derive(Debug, Clone)]
pub struct Options {
    /// Root of the new tree. Must be on the same filesystem as the files.
    pub root: PathBuf,
    pub gap_secs: i64,
    /// Refuse to move anything a live Lightroom catalog points at: moving it
    /// breaks the link, and the catalog is someone's work.
    pub respect_lightroom: bool,
    /// Leave files whose date is a guess exactly where they are.
    pub skip_uncertain: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            gap_secs: events::DEFAULT_GAP_SECS,
            respect_lightroom: true,
            skip_uncertain: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Move {
    pub file_id: i64,
    pub src: String,
    pub dst: String,
    pub size: i64,
    pub mtime: i64,
    pub date: Dated,
    /// Name of the event directory this frame lands in.
    pub event: String,
    /// Set when the name had to change because something else in the
    /// destination already carried it — Sony reuses `DSC0xxxx` endlessly.
    pub renamed_from: Option<String>,
}

impl Move {
    pub fn name(&self) -> &str {
        self.dst.rsplit('/').next().unwrap_or(&self.dst)
    }
}

#[derive(Debug, Clone)]
pub struct Refusal {
    pub path: String,
    pub why: String,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub moves: Vec<Move>,
    pub refusals: Vec<Refusal>,
    /// Files already sitting where the plan would put them.
    pub already_placed: usize,
    pub by_source: BTreeMap<Source, usize>,
    pub events: usize,
    pub renamed: usize,
    pub uncertain: usize,
}

impl Plan {
    pub fn bytes(&self) -> i64 {
        self.moves.iter().map(|m| m.size).sum()
    }
}

/// Hands out destination names, keeping a raw file and the JPEG beside it
/// under one stem even when that stem has to change.
#[derive(Default)]
struct Namer {
    taken: HashSet<(String, String)>,
    /// `(destination, source directory, stem)` — the source directory is what
    /// separates two cameras that both wrote `DSC01234` from a raw and a
    /// JPEG that are two halves of one shot.
    assigned: HashMap<(String, String, String), String>,
}

fn split_name(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

impl Namer {
    fn reserve(&mut self, dir: &str, src_dir: &str, name: &str) {
        let (stem, _) = split_name(name);
        self.taken.insert((dir.to_string(), stem.to_string()));
        self.assigned.insert(
            (dir.to_string(), src_dir.to_string(), stem.to_string()),
            stem.to_string(),
        );
    }

    fn assign(&mut self, dir: &str, src_dir: &str, name: &str) -> (String, bool) {
        let (stem, ext) = split_name(name);
        let key = (dir.to_string(), src_dir.to_string(), stem.to_string());
        if let Some(chosen) = self.assigned.get(&key) {
            let renamed = chosen != stem;
            return (join_name(chosen, ext), renamed);
        }

        let mut n = 1;
        let chosen = loop {
            let candidate = if n == 1 {
                stem.to_string()
            } else {
                format!("{stem}_{n}")
            };
            let free = !self.taken.contains(&(dir.to_string(), candidate.clone()))
                && !Path::new(dir).join(join_name(&candidate, ext)).exists();
            if free {
                break candidate;
            }
            n += 1;
        };

        self.taken.insert((dir.to_string(), chosen.clone()));
        self.assigned.insert(key, chosen.clone());
        let renamed = chosen != stem;
        (join_name(&chosen, ext), renamed)
    }
}

fn join_name(stem: &str, ext: &str) -> String {
    if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(a, _)| a)
}

/// Destination directory for a date the tool is not sure about.
fn coarse_dir(root: &Path, d: &Dated) -> (PathBuf, String) {
    let (y, m, ..) = pc_core::time::civil_from_unix(d.ts);
    match d.precision {
        Precision::Month => {
            let name = format!("{y:04}-{m:02}_{UNDATED_DIR}");
            (root.join(format!("{y:04}")).join(&name), name)
        }
        Precision::Year => (
            root.join(format!("{y:04}")).join(UNDATED_DIR),
            UNDATED_DIR.to_string(),
        ),
        // Reached only for a file whose mtime is not a date either.
        Precision::Day => (root.join(NO_DATE_DIR), NO_DATE_DIR.to_string()),
    }
}

pub fn compute(db: &Db, o: &Options) -> Result<Plan> {
    let root = o.root.clone();
    let root_dev = pc_core::dev_of_nearest_existing(&root).with_context(|| {
        format!(
            "не определить файловую систему для {} — каталог и его предки недоступны",
            root.display()
        )
    })?;

    let protected = if o.respect_lightroom {
        CurationIndex::build(db.lightroom_protected()?)
    } else {
        CurationIndex::default()
    };

    let mut plan = Plan::default();
    let mut dated: Vec<(OrganizeRow, Dated)> = Vec::new();

    for row in db.organize_rows()? {
        if row.dev as u64 != root_dev {
            plan.refusals.push(Refusal {
                path: row.path.clone(),
                why: format!(
                    "другой диск ({}) — перенос стал бы копированием; \
                     нужен свой прогон с --root на этом диске",
                    row.disk
                ),
            });
            continue;
        }
        let d = date::resolve(&row);
        if o.skip_uncertain && d.uncertain() {
            plan.refusals.push(Refusal {
                path: row.path.clone(),
                why: format!("дата ненадёжна, источник — {}", d.source.label()),
            });
            continue;
        }
        if let Some(c) = protected.lookup(&row.path) {
            let stars = c
                .rating
                .filter(|r| *r > 0)
                .map(|r| format!(", {r} звёзд"))
                .unwrap_or_default();
            plan.refusals.push(Refusal {
                path: row.path.clone(),
                why: format!("файл в каталоге Lightroom{stars} — перенос разорвал бы ссылку"),
            });
            continue;
        }
        *plan.by_source.entry(d.source).or_default() += 1;
        if d.uncertain() {
            plan.uncertain += 1;
        }
        dated.push((row, d));
    }

    // Deterministic order: the same archive must produce the same tree, and
    // collision suffixes must not depend on the order SQLite handed rows out.
    dated.sort_by(|(ra, da), (rb, db_)| da.ts.cmp(&db_.ts).then_with(|| ra.path.cmp(&rb.path)));

    // Directory per file, before any name is assigned.
    let mut placed: Vec<(OrganizeRow, Dated, PathBuf, String)> = Vec::new();

    let day: Vec<usize> = (0..dated.len())
        .filter(|i| dated[*i].1.precision == Precision::Day && dated[*i].1.ts > 0)
        .collect();
    let day_ts: Vec<i64> = day.iter().map(|i| dated[*i].1.ts).collect();

    let mut per_day: HashMap<String, usize> = HashMap::new();
    for range in events::split(&day_ts, o.gap_secs) {
        let start = day_ts[range.start];
        let date = pc_core::time::fmt_iso_date(start);
        let nth = per_day.entry(date.clone()).or_insert(0);
        let name = events::dir_name(start, *nth);
        *nth += 1;
        plan.events += 1;

        let (y, ..) = pc_core::time::civil_from_unix(start);
        let dir = root.join(format!("{y:04}")).join(&name);
        for idx in range {
            let (row, d) = dated[day[idx]].clone();
            placed.push((row, d, dir.clone(), name.clone()));
        }
    }

    for (i, (row, d)) in dated.iter().enumerate() {
        if day.binary_search(&i).is_ok() {
            continue;
        }
        let (dir, name) = coarse_dir(&root, d);
        placed.push((row.clone(), *d, dir, name));
    }

    // Names last, so that every directory is known before anything competes
    // for a name inside it.
    let mut namer = Namer::default();
    let mut moves = Vec::new();
    for (row, d, dir, event) in &placed {
        let dir_str = dir.to_string_lossy().into_owned();
        if dir_of(&row.path) == dir_str {
            namer.reserve(&dir_str, dir_of(&row.path), &row.name);
            plan.already_placed += 1;
            continue;
        }
        moves.push((row.clone(), *d, dir_str, event.clone()));
    }

    for (row, d, dir_str, event) in moves {
        let (name, renamed) = namer.assign(&dir_str, dir_of(&row.path), &row.name);
        if renamed {
            plan.renamed += 1;
        }
        plan.moves.push(Move {
            file_id: row.id,
            src: row.path.clone(),
            dst: format!("{dir_str}/{name}"),
            size: row.size,
            mtime: row.mtime,
            date: d,
            event,
            renamed_from: renamed.then(|| row.name.clone()),
        });
    }

    plan.moves.sort_by(|a, b| a.dst.cmp(&b.dst));
    plan.refusals.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_kept_unless_something_already_holds_it() {
        let mut n = Namer::default();
        let (a, renamed) = n.assign("/dst/2019/2019-07-14", "/src/a", "DSC01234.ARW");
        assert_eq!(a, "DSC01234.ARW");
        assert!(!renamed);

        // Same stem, different camera and folder: must not collide.
        let (b, renamed) = n.assign("/dst/2019/2019-07-14", "/src/b", "DSC01234.ARW");
        assert_eq!(b, "DSC01234_2.ARW");
        assert!(renamed);
    }

    #[test]
    fn a_raw_and_its_jpeg_keep_one_stem_even_when_it_changes() {
        let mut n = Namer::default();
        n.assign("/dst/e", "/src/a", "DSC01234.ARW");
        let (jpg, _) = n.assign("/dst/e", "/src/b", "DSC01234.JPG");
        assert_eq!(jpg, "DSC01234_2.JPG");
        let (raw2, _) = n.assign("/dst/e", "/src/b", "DSC01234.ARW");
        assert_eq!(
            raw2, "DSC01234_2.ARW",
            "кадр и его JPEG обязаны остаться одной парой"
        );
    }

    #[test]
    fn a_file_without_an_extension_still_gets_a_name() {
        let mut n = Namer::default();
        assert_eq!(n.assign("/dst/e", "/src", "IMG_0001").0, "IMG_0001");
        assert_eq!(n.assign("/dst/e", "/other", "IMG_0001").0, "IMG_0001_2");
    }

    #[test]
    fn uncertain_dates_go_to_their_own_corner() {
        let root = PathBuf::from("/dst");
        let month = Dated {
            ts: 1_562_000_000,
            source: Source::Path,
            precision: Precision::Month,
        };
        let (dir, _) = coarse_dir(&root, &month);
        assert_eq!(dir, PathBuf::from("/dst/2019/2019-07_без-точной-даты"));

        let year = Dated {
            precision: Precision::Year,
            ..month
        };
        let (dir, _) = coarse_dir(&root, &year);
        assert_eq!(dir, PathBuf::from("/dst/2019/без-точной-даты"));
    }
}
