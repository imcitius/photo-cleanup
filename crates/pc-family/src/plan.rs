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

pub fn compute(db: &Db, policy: &Policy) -> Result<Plan> {
    let rows = db.plan_rows()?;
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
                    why: "единственный файл в семействе".into(),
                });
                continue;
            }
            if role == Role::Resize && m.width * m.height >= policy.resize_below_pixels {
                continue;
            }
            if let Some(c) = protected.lookup(&m.path) {
                let stars = c
                    .rating
                    .filter(|r| *r > 0)
                    .map(|r| format!(", {r} звёзд"))
                    .unwrap_or_default();
                let how = if c.exact {
                    ""
                } else {
                    " (совпадение по пути)"
                };
                plan.refusals.push(Refusal {
                    path: m.path.clone(),
                    why: format!("файл в каталоге Lightroom{stars}{how}"),
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
                    let name = keeper
                        .path
                        .rsplit_once('/')
                        .map_or(keeper.path.as_str(), |(_, b)| b);
                    // Only a copy is guaranteed to be the same pixels. For
                    // the other roles the honest claim is weaker, and saying
                    // more than is true is how a user agrees to something
                    // they did not mean to.
                    match role {
                        Role::Copy => {
                            format!("точная копия, те же пиксели сохранены в {name}")
                        }
                        Role::Resize => format!(
                            "уменьшенная версия {}×{}, полный кадр остаётся в {name}",
                            m.width, m.height
                        ),
                        other => format!(
                            "{} — в семействе остаётся {name}",
                            other.label()
                        ),
                    }
                },
            });
        }
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
