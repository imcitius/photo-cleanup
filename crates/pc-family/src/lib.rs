//! Grouping an archive into families: one photograph, several renditions.

pub mod links;
pub mod perceptual;
pub mod quality;
pub mod roles;
pub mod unionfind;

pub use links::{Link, LinkKind};
pub use perceptual::Params;
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
    let exact = links::detect(&files);
    report.exact_links = exact.len();
    for l in &exact {
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
    let cands = perceptual::candidates(&files, params);
    report.perceptual_candidates = cands.len();
    let verdict = perceptual::verify(&files, &cands, store, params);
    report.rejected_by_ssim = verdict.rejected_by_ssim;
    report.rejected_as_blank = verdict.rejected_as_blank;
    report.rejected_as_series = verdict.rejected_as_series;
    report.perceptual_verified = verdict.verified.len();
    for v in &verdict.verified {
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
        if pc_hash::hamming(files[a].phash, files[b].phash) <= params.phash_max {
            uf.union(a, b);
        }
    }

    // --- write out --------------------------------------------------------
    let groups = uf.groups();
    db.clear_families()?;
    let run_id = db.latest_run()?.unwrap_or(0);

    db.conn.execute_batch("BEGIN")?;
    for members in groups.values() {
        let best_pixels = members
            .iter()
            .map(|&m| files[m].pixels())
            .max()
            .unwrap_or(0);
        let scores: Vec<quality::Score> = members
            .iter()
            .map(|&m| quality::score(&files[m], best_pixels))
            .collect();
        let totals: Vec<f64> = scores.iter().map(|s| s.total).collect();
        let member_roles = roles::assign(&files, members, &totals);

        // Roles exist only now, so the keeper is chosen here rather than from
        // the raw quality score: an original outranks a larger export.
        let keeper_pos = (0..members.len())
            .max_by(|&x, &y| {
                let vx = totals[x] + roles::keeper_bonus(member_roles[x]);
                let vy = totals[y] + roles::keeper_bonus(member_roles[y]);
                vx.partial_cmp(&vy).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(0);

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
    db.conn.execute_batch("COMMIT")?;

    Ok(report)
}
