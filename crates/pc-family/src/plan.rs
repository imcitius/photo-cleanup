//! Turning roles and a policy into a concrete list of files to move.
//!
//! Every refusal is recorded next to its reason. A plan that silently drops
//! candidates is one the user cannot audit, and auditing is the only thing
//! standing between this tool and somebody's photographs.

use anyhow::Result;
use pc_db::{Db, PlanRow};
use std::collections::{BTreeSet, HashMap};

use crate::curation::CurationIndex;
use crate::Role;

#[derive(Debug, Clone)]
pub struct Policy {
    /// Roles the user has agreed to part with.
    pub remove_roles: BTreeSet<Role>,
    /// A `resize` is only a candidate below this many pixels.
    pub resize_below_pixels: i64,
    /// Refuse to touch anything a live Lightroom catalog references.
    pub respect_lightroom: bool,
}

impl Default for Policy {
    fn default() -> Self {
        // Only exact copies, and nothing a catalog knows about. Everything
        // else is a decision the user has to make deliberately.
        Self {
            remove_roles: [Role::Copy].into_iter().collect(),
            resize_below_pixels: 2_000_000,
            respect_lightroom: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub file_id: i64,
    pub family_id: i64,
    pub path: String,
    pub size: i64,
    pub role: Role,
    /// The member whose survival makes removing this one safe.
    pub keeper_id: i64,
    pub keeper_path: String,
    pub reason: String,
    /// Chosen by the user rather than derived from a role, so the interface
    /// can say which of the two put each file on the list.
    pub manual: bool,
}

#[derive(Debug, Clone)]
pub struct Refusal {
    pub path: String,
    pub why: String,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub candidates: Vec<Candidate>,
    pub refusals: Vec<Refusal>,
}

impl Plan {
    pub fn bytes(&self) -> i64 {
        self.candidates.iter().map(|c| c.size).sum()
    }
}

fn keeper_of(members: &[PlanRow]) -> Option<&PlanRow> {
    members
        .iter()
        .find(|m| m.is_keeper)
        .or_else(|| members.iter().max_by_key(|m| m.width * m.height))
}

/// What the plan is being asked about: the whole archive, one group, or one
/// folder. Acting on a group should cost what that group costs.
#[derive(Debug, Default, Clone)]
pub struct Scope {
    pub family: Option<i64>,
    /// Matched by prefix when the rows are fetched; the exact folder is the
    /// caller's business.
    pub folder: Option<String>,
    /// Groups whose *kept* file lives here. This is the other direction: not
    /// "clear out this folder" but "the originals are here, take away what
    /// duplicates them, wherever it lies".
    pub keeper_folder: Option<String>,
}

/// What a file has to match to be called a copy of another: the whole frame
/// in colour, falling back to the old grey square only for rows written
/// before that evidence existed.
fn identity(r: &PlanRow) -> Option<&Vec<u8>> {
    r.content_hash.as_ref().or(r.pixel_hash.as_ref())
}

pub fn compute(db: &Db, policy: &Policy) -> Result<Plan> {
    compute_scoped(db, policy, &Scope::default())
}

pub fn compute_scoped(db: &Db, policy: &Policy, scope: &Scope) -> Result<Plan> {
    let rows = db.plan_rows_scoped(
        scope.family,
        scope.folder.as_deref(),
        scope.keeper_folder.as_deref(),
    )?;
    let protected = if policy.respect_lightroom {
        CurationIndex::build(db.lightroom_protected()?)
    } else {
        CurationIndex::default()
    };

    let mut by_family: HashMap<i64, Vec<PlanRow>> = HashMap::new();
    for r in rows {
        by_family.entry(r.family_id).or_default().push(r);
    }

    let mut plan = Plan::default();
    for (_family, members) in by_family {
        let Some(keeper) = keeper_of(&members) else {
            continue;
        };

        for m in &members {
            let role = Role::parse(&m.role).unwrap_or(Role::Unknown);

            if !policy.remove_roles.contains(&role) {
                continue;
            }
            // The keeper is what makes removing the others safe.
            if m.file_id == keeper.file_id {
                continue;
            }
            // A family of one has nothing to fall back on.
            if members.len() < 2 {
                plan.refusals.push(Refusal {
                    path: m.path.clone(),
                    why: pc_core::tr!(
                        "единственный файл в семействе",
                        "the only file in its group"
                    )
                    .into(),
                });
                continue;
            }
            if role == Role::Resize && m.width * m.height >= policy.resize_below_pixels {
                continue;
            }
            // A copy is a copy *of the file being kept*. The roles were
            // settled when the group was built, against whichever member was
            // kept then — and the user is free to keep a different one since,
            // by hand or by naming a folder. A scan and the JPEG exported
            // from it belong to one group and do not share a pixel: if the
            // export is what stays, the scans are no longer copies of it.
            //
            // Proposing them anyway wastes the only safety that matters: the
            // move re-reads both files, sees two different pictures and
            // refuses — every time, for ever, with the group stuck in the
            // list. So the plan asks the question here instead.
            if role == Role::Copy {
                let same = match (identity(m), identity(keeper)) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                if !same {
                    plan.refusals.push(Refusal {
                        path: m.path.clone(),
                        why: pc_core::tf!(
                            "не совпадает с сохраняемым файлом: {0}",
                            "does not match the file being kept: {0}",
                            keeper.path
                        ),
                    });
                    continue;
                }
            }
            if let Some(c) = protected.lookup(&m.path) {
                let stars = c
                    .rating
                    .filter(|r| *r > 0)
                    .map(|r| pc_core::tf!(", {0} звёзд", ", {0} stars", r))
                    .unwrap_or_default();
                let how = if c.exact {
                    ""
                } else {
                    pc_core::tr!(" (совпадение по пути)", " (matched by path)")
                };
                plan.refusals.push(Refusal {
                    path: m.path.clone(),
                    why: pc_core::tf!(
                        "файл в каталоге Lightroom{0}{1}",
                        "the file is in a Lightroom catalogue{0}{1}",
                        stars,
                        how
                    ),
                });
                continue;
            }

            plan.candidates.push(Candidate {
                file_id: m.file_id,
                family_id: m.family_id,
                path: m.path.clone(),
                size: m.size,
                role,
                keeper_id: keeper.file_id,
                keeper_path: keeper.path.clone(),
                reason: {
                    let name = pc_core::base_name(&keeper.path);
                    // Only a copy is guaranteed to be the same pixels. For
                    // the other roles the honest claim is weaker, and saying
                    // more than is true is how a user agrees to something
                    // they did not mean to.
                    match role {
                        Role::Copy => {
                            pc_core::tf!(
                                "точная копия, те же пиксели сохранены в {0}",
                                "an exact copy; the same pixels are kept in {0}",
                                name
                            )
                        }
                        Role::Resize => pc_core::tf!(
                            "уменьшенная версия {0}×{1}, полный кадр остаётся в {2}",
                            "a {0}×{1} reduction; the full frame stays in {2}",
                            m.width,
                            m.height,
                            name
                        ),
                        other => pc_core::tf!(
                            "{0} — в семействе остаётся {1}",
                            "{0} — {1} stays in the group",
                            other.label(),
                            name
                        ),
                    }
                },
                manual: false,
            });
        }
    }

    // Frames the user rejected by hand. These arrive with no family and no
    // keeper behind them: the argument for moving them is not "a better copy
    // survives" but "the person looked at it and said no". Saying anything
    // stronger in the reason column would be inventing a justification the
    // tool does not have.
    let already: BTreeSet<i64> = plan.candidates.iter().map(|c| c.file_id).collect();
    for m in db.rejected_rows_scoped(scope.family)? {
        if already.contains(&m.file_id) {
            continue;
        }
        if let Some(c) = protected.lookup(&m.path) {
            let stars = c
                .rating
                .filter(|r| *r > 0)
                .map(|r| pc_core::tf!(", {0} звёзд", ", {0} stars", r))
                .unwrap_or_default();
            plan.refusals.push(Refusal {
                path: m.path.clone(),
                why: pc_core::tf!(
                    "отклонён вручную, но файл в каталоге Lightroom{0}",
                    "rejected by hand, but the file is in a Lightroom catalogue{0}",
                    stars
                ),
            });
            continue;
        }
        plan.candidates.push(Candidate {
            file_id: m.file_id,
            family_id: m.family_id,
            path: m.path.clone(),
            size: m.size,
            role: Role::Unknown,
            keeper_id: 0,
            keeper_path: String::new(),
            reason: pc_core::tr!(
                "отклонён вручную при разборе серии",
                "rejected by hand while sorting a burst"
            )
            .into(),
            manual: true,
        });
    }

    plan.candidates.sort_by_key(|c| std::cmp::Reverse(c.size));
    plan.refusals.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_policy_only_takes_exact_copies() {
        let p = Policy::default();
        assert!(p.remove_roles.contains(&Role::Copy));
        for r in [
            Role::Original,
            Role::CameraJpeg,
            Role::Converted,
            Role::Export,
            Role::Resize,
            Role::Unknown,
        ] {
            assert!(
                !p.remove_roles.contains(&r),
                "{r:?} удалялась бы по умолчанию"
            );
        }
        assert!(p.respect_lightroom);
    }

    fn row(id: i64, role: &str, keeper: bool, path: &str) -> PlanRow {
        PlanRow {
            family_id: 1,
            file_id: id,
            role: role.into(),
            path: path.into(),
            size: 1000,
            width: 4000,
            height: 3000,
            mtime: 0,
            inode: id,
            dev: 1,
            disk: "d".into(),
            pixel_hash: Some(vec![1; 32]),
            content_hash: Some(vec![1; 32]),
            is_keeper: keeper,
        }
    }

    #[test]
    fn the_keeper_is_what_makes_the_rest_safe_to_move() {
        let members = vec![
            row(1, "original", true, "/a/keep.jpg"),
            row(2, "copy", false, "/b/copy.jpg"),
        ];
        let k = keeper_of(&members).unwrap();
        assert_eq!(k.file_id, 1);
    }

    #[test]
    fn a_family_without_a_marked_keeper_falls_back_to_the_largest() {
        let mut members = vec![
            row(1, "copy", false, "/a/small.jpg"),
            row(2, "copy", false, "/b/big.jpg"),
        ];
        members[0].width = 100;
        members[0].height = 100;
        assert_eq!(keeper_of(&members).unwrap().file_id, 2);
    }
}

#[cfg(test)]
mod keeper_tests {
    use super::*;
    use pc_db::files::NewFile;

    fn file(db: &Db, run: i64, path: &str, pixels: u8) -> i64 {
        db.upsert_file(
            &NewFile {
                path: path.into(),
                name: pc_core::base_name(path).into(),
                disk: "disk1".into(),
                size: 1000,
                mtime: 1,
                container: Some("jpeg".into()),
                pixel_hash: Some(vec![pixels; 32]),
                phash: Some(1),
                thumb_key: Some("k".into()),
                ..Default::default()
            },
            run,
        )
        .unwrap()
    }

    /// A group is one photograph, which is not the same as one set of pixels:
    /// a scan and the JPEG exported from it belong together and share nothing
    /// byte for byte. Whichever of them is kept, the other is not a copy of
    /// it — and the plan has to say so before the move re-reads both files,
    /// sees two different pictures and refuses.
    #[test]
    fn a_copy_of_something_else_is_not_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("pc.db")).unwrap();
        let run = db.start_run(&[], "test").unwrap();

        let scan = file(&db, run, "/foto/N.66.BMP", 1);
        let export = file(&db, run, "/foto/N.66.jpg", 9);
        let twin = file(&db, run, "/backup/N.66.BMP", 1);

        // The export is what is kept — by hand, or because a folder was
        // named the main one.
        let family = db
            .insert_family("linked", None, None, Some(export), run)
            .unwrap();
        for (f, role) in [(scan, "original"), (export, "export"), (twin, "copy")] {
            db.insert_family_member(family, f, role, None, 1.0, "")
                .unwrap();
        }

        let plan = compute(&db, &Policy::default()).unwrap();
        assert!(
            plan.candidates.is_empty(),
            "предложено к переносу: {:?}",
            plan.candidates.iter().map(|c| &c.path).collect::<Vec<_>>()
        );
        assert!(
            plan.refusals.iter().any(|r| r.path.contains("backup")),
            "отказ не объяснён: {:?}",
            plan.refusals
        );
    }

    #[test]
    fn a_copy_of_the_kept_file_still_is_one() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("pc.db")).unwrap();
        let run = db.start_run(&[], "test").unwrap();

        let scan = file(&db, run, "/foto/N.66.BMP", 1);
        let twin = file(&db, run, "/backup/N.66.BMP", 1);
        let family = db
            .insert_family("linked", None, None, Some(scan), run)
            .unwrap();
        for (f, role) in [(scan, "original"), (twin, "copy")] {
            db.insert_family_member(family, f, role, None, 1.0, "")
                .unwrap();
        }

        let plan = compute(&db, &Policy::default()).unwrap();
        assert_eq!(plan.candidates.len(), 1, "{:?}", plan.candidates);
        assert!(plan.candidates[0].path.contains("backup"));
    }
}
