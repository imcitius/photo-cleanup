//! Grouping an archive into families: one photograph, several renditions.

pub mod categories;
pub mod curation;
pub mod folders;
pub mod links;
pub mod perceptual;
pub mod plan;
pub mod quality;
pub mod roles;
pub mod series;
pub mod unionfind;

pub use links::{Link, LinkKind};
pub use perceptual::Params;
pub use plan::{Plan, Policy};
pub use roles::Role;

use anyhow::Result;
use pc_core::ThumbStore;
use pc_db::Db;
use std::collections::BTreeMap;
use unionfind::UnionFind;

#[derive(Debug, Default)]
pub struct BuildReport {
    pub files: usize,
    pub families: usize,
    pub multi_member: usize,
    pub exact_links: usize,
    pub perceptual_candidates: usize,
    pub perceptual_verified: usize,
    pub rejected_by_ssim: u64,
    pub rejected_as_blank: u64,
    pub rejected_as_series: u64,
    pub by_role: BTreeMap<&'static str, usize>,
    pub removable_files: usize,
    pub removable_bytes: i64,
}

/// Evidence for why two files ended up together, kept so the interface can
/// justify every grouping it shows.
#[derive(Debug, Clone, serde::Serialize)]
struct Evidence {
    kind: &'static str,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssim: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phash_distance: Option<u32>,
}

/// Build families from exact links first, then fill the gaps perceptually.
pub fn build(db: &Db, store: &ThumbStore, params: &Params) -> Result<BuildReport> {
    build_controlled(db, store, params, &pc_core::work::Control::default())
}
pub fn build_controlled(
    db: &Db,
    store: &ThumbStore,
    params: &Params,
    control: &pc_core::work::Control,
) -> Result<BuildReport> {
    let files = db.all_indexed()?;
    let mut report = BuildReport {
        files: files.len(),
        ..Default::default()
    };
    if files.is_empty() {
        return Ok(report);
    }

    let mut uf = UnionFind::new(files.len());
    let mut evidence: BTreeMap<usize, Evidence> = BTreeMap::new();

    // --- what the files themselves assert --------------------------------
    let split_ids: std::collections::HashSet<i64> = db
        .conn
        .prepare("SELECT file_id FROM manual_splits")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let manual_keepers: std::collections::HashSet<i64> = db
        .conn
        .prepare("SELECT file_id FROM manual_keepers")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let may_join =
        |a: usize, b: usize| !split_ids.contains(&files[a].id) && !split_ids.contains(&files[b].id);
    let exact = links::detect(&files);
    report.exact_links = exact.len();
    for l in &exact {
        if !may_join(l.a, l.b) {
            continue;
        }
        uf.union(l.a, l.b);
        for idx in [l.a, l.b] {
            evidence.entry(idx).or_insert(Evidence {
                kind: l.kind.as_str(),
                detail: l.detail.clone(),
                ssim: None,
                phash_distance: None,
            });
        }
    }

    // --- what they only look like ----------------------------------------
    control.begin(
        pc_core::tr!("Поиск похожих изображений", "Looking for similar images"),
        files.len() as u64,
        0,
    )?;
    let cands = perceptual::candidates_controlled(&files, params, control);
    control.check()?;
    report.perceptual_candidates = cands.len();
    control.begin(
        pc_core::tr!(
            "Проверка сходства по пикселям",
            "Checking similarity on pixels"
        ),
        cands.len() as u64,
        0,
    )?;
    let verdict = perceptual::verify_controlled(&files, &cands, store, params, control);
    control.check()?;
    report.rejected_by_ssim = verdict.rejected_by_ssim;
    report.rejected_as_blank = verdict.rejected_as_blank;
    report.rejected_as_series = verdict.rejected_as_series;
    report.perceptual_verified = verdict.verified.len();
    for v in &verdict.verified {
        if !may_join(v.a, v.b) {
            continue;
        }
        uf.union(v.a, v.b);
        for idx in [v.a, v.b] {
            evidence.entry(idx).or_insert(Evidence {
                kind: if v.via_crop { "crop" } else { "perceptual" },
                detail: format!("SSIM {:.3}, pHash {}", v.ssim, v.phash_distance),
                ssim: Some(v.ssim),
                phash_distance: Some(v.phash_distance),
            });
        }
    }

    // Same body and second, but only where appearance already agrees.
    let same_second = links::shutter_candidates(&files);
    for (a, b) in same_second {
        if may_join(a, b) && pc_hash::hamming(files[a].phash, files[b].phash) <= params.phash_max {
            uf.union(a, b);
        }
    }

    // --- write out --------------------------------------------------------
    // What the photographer already curated. This is both a protection and
    // the strongest quality signal there is, so it belongs in the score.
    let curated = curation::CurationIndex::build(db.lightroom_protected()?);

    let groups = uf.groups();
    control.begin(
        pc_core::tr!("Сохранение семейств", "Saving the groups"),
        groups.len() as u64,
        0,
    )?;
    db.conn.execute_batch("BEGIN")?;
    db.clear_families()?;
    let run_id = db.latest_run()?.unwrap_or(0);

    for members in groups.values() {
        control.check()?;
        control.advance(0, None);
        let best_pixels = members
            .iter()
            .map(|&m| files[m].pixels())
            .max()
            .unwrap_or(0);
        let scores: Vec<quality::Score> = members
            .iter()
            .map(|&m| {
                let c = curated
                    .lookup(&files[m].path)
                    .map(|k| quality::Curation {
                        in_catalog: true,
                        rating: k.rating,
                    })
                    .unwrap_or_default();
                quality::score(&files[m], best_pixels, c)
            })
            .collect();
        let totals: Vec<f64> = scores.iter().map(|s| s.total).collect();
        let member_roles = roles::assign(&files, members, &totals);

        // Roles exist only now, so the keeper is chosen here rather than from
        // the raw quality score: an original outranks a larger export.
        //
        // Ties are broken deliberately rather than by iteration order. Three
        // byte-identical copies score the same to the decimal, and letting
        // chance decide which one is "the original" means the tool can offer
        // to move the file in its proper place and keep the one in a backup
        // folder — which is exactly backwards.
        let keeper_pos = (0..members.len())
            .max_by(|&x, &y| {
                let key = |i: usize| {
                    let f = &files[members[i]];
                    (
                        totals[i] + roles::keeper_bonus(member_roles[i]),
                        // Shallower paths are the working copy; deeper ones
                        // tend to be archives of it.
                        -(pc_core::path_parts(&f.path).len() as f64),
                        -(f.path.len() as f64),
                    )
                };
                let (a, b) = (key(x), key(y));
                a.partial_cmp(&b)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    // Final fallback: the path itself, so the answer never
                    // changes between runs over the same archive.
                    .then_with(|| files[members[y]].path.cmp(&files[members[x]].path))
            })
            .unwrap_or(0);

        let keeper_pos = members
            .iter()
            .position(|&m| manual_keepers.contains(&files[m].id))
            .unwrap_or(keeper_pos);
        let rep = &files[members[keeper_pos]];
        let kind = if members.len() == 1 {
            "single"
        } else if members.iter().any(|m| evidence.contains_key(m)) {
            "linked"
        } else {
            "perceptual"
        };

        let family_id = db.insert_family(
            kind,
            rep.taken_at,
            rep.camera_model.as_deref(),
            Some(files[members[keeper_pos]].id),
            run_id,
        )?;

        for (pos, &m) in members.iter().enumerate() {
            let role = member_roles[pos];
            *report.by_role.entry(role.as_str()).or_insert(0) += 1;
            if role.removable_by_default() {
                report.removable_files += 1;
                report.removable_bytes += files[m].size;
            }
            let ev = evidence
                .get(&m)
                .map(|e| serde_json::to_string(e).unwrap_or_default());
            db.insert_family_member(
                family_id,
                files[m].id,
                role.as_str(),
                ev.as_deref(),
                totals[pos],
                &scores[pos].explain(),
            )?;
        }

        report.families += 1;
        if members.len() > 1 {
            report.multi_member += 1;
        }
    }
    // Rebuilding the groups picks every keeper afresh, so the folder a burst
    // was settled into has to be settled again — otherwise the sequence
    // scatters back across folders on the next run.
    folders::settle_keepers(db)?;
    db.conn.execute_batch("COMMIT")?;

    Ok(report)
}
