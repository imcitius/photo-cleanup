//! Folders named as holding the archive's originals.
//!
//! An archive is a tree, and the thing a person knows about it is usually a
//! statement about a tree: "the photographs live under `/foto`, everything in
//! `/backup` is a copy of them". Said once, that settles ten thousand groups
//! — but only if it is kept as a rule rather than spent as a press, because
//! the groups are rebuilt whenever the archive is read again, and files
//! indexed tomorrow are inside the same folder.
//!
//! So the marks live in the database and this runs after every rebuild. What
//! a mark covers — one folder, or one path under every root of an array — is
//! `pc_db::Marks`; this only asks it.

use anyhow::Result;
use pc_db::{Db, KeeperSource};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Groups the marked folders have a file in. Every one of them, whatever
    /// happened next — this is the number the interface calls "groups with a
    /// file here", and it has to mean that.
    pub groups: u64,
    /// Groups whose kept file this moved into a marked folder.
    pub moved: u64,
    /// Groups where the marked folder holds a *different* picture, not a copy
    /// of the one being kept — a scan and the JPEG exported from it. Nothing
    /// measurable settles those, so the rule leaves them for a person.
    pub untouched: u64,
    /// Groups the rule did not touch because a person had already answered
    /// them, by pressing "keep this one" or by setting the file aside.
    pub by_hand: u64,
}

struct Member {
    file_id: i64,
    path: String,
    quality: f64,
    frame: Option<Vec<u8>>,
    source: Option<String>,
    bytes: Option<Vec<u8>>,
    keeper: Option<i64>,
    keeper_frame: Option<Vec<u8>>,
    keeper_source: Option<String>,
    keeper_bytes: Option<Vec<u8>>,
}

impl Member {
    /// What this file has to match to be a copy of what the group keeps, and
    /// what the kept file offers in return. One model for both — the same one
    /// the roles and the plan use.
    fn pair(&self) -> Option<(pc_db::Identity<'_>, pc_db::Identity<'_>)> {
        Some((
            pc_db::Identity::of(
                self.frame.as_ref(),
                None,
                self.source.as_deref(),
                self.bytes.as_ref(),
            )?,
            pc_db::Identity::of(
                self.keeper_frame.as_ref(),
                None,
                self.keeper_source.as_deref(),
                self.keeper_bytes.as_ref(),
            )?,
        ))
    }
}

/// Apply every mark, for every group.
///
/// Safe to run when there are no marks and safe to run twice: it writes the
/// same answer each time, and a group whose kept file a person chose is never
/// touched at all.
///
/// No transaction of its own: it runs at the end of a rebuild, inside the one
/// that wrote the groups, and opening a second there is an error.
pub fn settle(db: &Db) -> Result<Report> {
    // First take back what no mark covers any more. The set of marks is not
    // the only thing that changes under a folder decision: editing the roots
    // on the settings screen changes what a relative mark reaches, and a
    // decision left over from the old reading would go on overriding the
    // automatic choice with nothing behind it.
    db.revoke_uncovered_folder_keepers()?;

    let marks = db.original_marks()?;
    if marks.is_empty() {
        return Ok(Report::default());
    }

    // Decisions a person made, which no rule overrules.
    let by_hand: HashSet<i64> = db
        .conn
        .prepare(
            "SELECT m.family_id FROM manual_keepers k
               JOIN family_members m ON m.file_id = k.file_id
              WHERE k.source = 'hand'",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // And frames a person set aside. A rule may not choose one of those, and
    // may not quietly take the rejection back: the whole promise of the mark
    // is that it does not overrule a human answer.
    let rejected: HashSet<i64> = db
        .conn
        .prepare("SELECT file_id FROM manual_rejects")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut by_family: HashMap<i64, Vec<Member>> = HashMap::new();
    {
        let mut st = db.conn.prepare(
            "SELECT fm.family_id, fm.file_id, f.path, COALESCE(fm.quality, 0),
                    COALESCE(f.content_hash, f.pixel_hash), f.pixel_source, f.partial_hash,
                    fa.keeper_file,
                    (SELECT COALESCE(k.content_hash, k.pixel_hash) FROM files k
                      WHERE k.id = fa.keeper_file),
                    (SELECT k.pixel_source FROM files k WHERE k.id = fa.keeper_file),
                    (SELECT k.partial_hash FROM files k WHERE k.id = fa.keeper_file)
               FROM family_members fm
               JOIN files f     ON f.id = fm.file_id
               JOIN families fa ON fa.id = fm.family_id
              WHERE f.state = 'present'",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                Member {
                    file_id: r.get(1)?,
                    path: r.get(2)?,
                    quality: r.get(3)?,
                    frame: r.get(4)?,
                    source: r.get(5)?,
                    bytes: r.get(6)?,
                    keeper: r.get(7)?,
                    keeper_frame: r.get(8)?,
                    keeper_source: r.get(9)?,
                    keeper_bytes: r.get(10)?,
                },
            ))
        })?;
        for row in rows {
            let (family, member) = row?;
            by_family.entry(family).or_default().push(member);
        }
    }

    let mut report = Report::default();
    for (family, members) in &by_family {
        // Whether the marks reach this group at all is asked before anything
        // else is decided about it: "groups with a file here" is a fact about
        // the archive, not about what the rule got to do.
        let covered = members.iter().any(|m| marks.covering(&m.path).is_some());
        if !covered {
            continue;
        }
        report.groups += 1;
        if by_hand.contains(family) {
            report.by_hand += 1;
            continue;
        }
        // How specific the mark covering this file is, in folders below the
        // root. Two marks can cover one file — a disk named on its own inside
        // a path named everywhere — and the deeper of them decides, so that
        // naming one disk more narrowly is a way of preferring it.
        let depth = |path: &str| {
            marks
                .covering(path)
                .map(|m| pc_core::path_parts(&m.path).len())
        };
        let mut best: Option<(usize, f64, i64)> = None;
        let mut matched = false;
        for m in members {
            let Some(depth) = depth(&m.path) else {
                continue;
            };
            if rejected.contains(&m.file_id) {
                continue;
            }
            // A group is one photograph, and that is not one set of pixels: a
            // scan and the JPEG exported from it belong together and share no
            // byte. Moving the kept file onto something that does not match
            // would leave every other member a "copy" of a picture it is not,
            // and the move refuses at the last moment, for ever. So the rule
            // only takes over the groups whose picture the folder holds.
            let same = m.pair().is_some_and(|(mine, kept)| mine == kept);
            if !same {
                continue;
            }
            matched = true;
            let candidate = (depth, m.quality, -m.file_id);
            if best.is_none_or(|b| candidate > b) {
                best = Some(candidate);
            }
        }
        if !matched {
            report.untouched += 1;
            continue;
        }
        let file = -best.expect("a match was found").2;
        let was = members.first().and_then(|m| m.keeper);
        if db.set_manual_keeper_from(*family, file, KeeperSource::Folder)? && was != Some(file) {
            report.moved += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_db::files::NewFile;
    use pc_db::MarkScope;

    fn paths(db: &Db) -> Vec<String> {
        let mut out: Vec<String> = db
            .original_marks()
            .unwrap()
            .marks
            .into_iter()
            .map(|m| m.path)
            .collect();
        out.sort();
        out
    }

    struct World {
        _tmp: tempfile::TempDir,
        db: Db,
        run: i64,
    }

    fn world() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("pc.db")).unwrap();
        let run = db.start_run(&[], "test").unwrap();
        World { _tmp: tmp, db, run }
    }

    fn file(w: &World, dir: &str, name: &str, pixels: u8) -> i64 {
        w.db.upsert_file(
            &NewFile {
                path: format!("{dir}/{name}"),
                name: name.into(),
                disk: "disk1".into(),
                size: 1000,
                mtime: 1,
                container: Some("jpeg".into()),
                pixel_hash: Some(vec![pixels; 32]),
                content_hash: Some(vec![pixels; 32]),
                partial_hash: Some(vec![pixels; 32]),
                pixel_source: Some("full".into()),
                phash: Some(1),
                dhash: Some(1),
                phash_canon: Some(1),
                width: Some(1000),
                height: Some(800),
                thumb_key: Some("k".into()),
                ..Default::default()
            },
            w.run,
        )
        .unwrap()
    }

    fn family(w: &World, keeper: i64, members: &[i64]) -> i64 {
        let id =
            w.db.insert_family("linked", None, None, Some(keeper), w.run)
                .unwrap();
        for &m in members {
            w.db.insert_family_member(id, m, "copy", None, 1.0, "")
                .unwrap();
        }
        id
    }

    fn keeper_of_file(w: &World, file: i64) -> bool {
        w.db.conn
            .query_row(
                "SELECT COUNT(*) FROM families WHERE keeper_file = ?1",
                [file],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }

    fn keeper_of(w: &World, family: i64) -> i64 {
        w.db.conn
            .query_row(
                "SELECT keeper_file FROM families WHERE id = ?1",
                [family],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_mark_reaches_every_depth_below_the_folder_it_names() {
        let w = world();
        let backup = file(&w, "/backup/2014", "DSC_0001.JPG", 1);
        let deep = file(&w, "/foto/2014/raw/june", "DSC_0001.JPG", 1);
        let fam = family(&w, backup, &[backup, deep]);

        w.db.mark_original("/foto", MarkScope::Absolute).unwrap();
        let report = settle(&w.db).unwrap();

        assert_eq!(
            report,
            Report {
                groups: 1,
                moved: 1,
                untouched: 0,
                by_hand: 0
            }
        );
        assert_eq!(keeper_of(&w, fam), deep);
    }

    #[test]
    fn one_relative_mark_settles_the_same_folder_on_every_disk() {
        // The Unraid shape: three filesystems, one structure laid across
        // them, and the copies of a shot scattered over all three.
        let w = world();
        w.db.conn
            .execute(
                "INSERT INTO settings(key, value) VALUES('roots', ?1)",
                ["[\"/mnt/disk1\",\"/mnt/disk2\",\"/mnt/disk3\"]"],
            )
            .unwrap();

        let mut families = Vec::new();
        for (n, disk) in ["disk1", "disk2", "disk3"].iter().enumerate() {
            let pixels = n as u8 + 1;
            let loose = file(&w, &format!("/mnt/{disk}/D/свалка"), "IMG.JPG", pixels);
            let good = file(
                &w,
                &format!("/mnt/{disk}/D/разобрано/даня/театр"),
                "IMG.JPG",
                pixels,
            );
            // Kept in the wrong place to begin with, on every disk.
            families.push((family(&w, loose, &[loose, good]), good));
        }

        w.db.mark_original("D/разобрано/даня/театр", MarkScope::EveryRoot)
            .unwrap();
        let report = settle(&w.db).unwrap();

        assert_eq!(
            report,
            Report {
                groups: 3,
                moved: 3,
                untouched: 0,
                by_hand: 0
            }
        );
        for (fam, good) in families {
            assert_eq!(keeper_of(&w, fam), good, "диск не подхвачен отметкой");
        }
        assert_eq!(paths(&w.db), ["D/разобрано/даня/театр"], "отметка одна");
    }

    #[test]
    fn a_disk_added_later_is_covered_without_marking_it_again() {
        let w = world();
        w.db.conn
            .execute(
                "INSERT INTO settings(key, value) VALUES('roots', ?1)",
                ["[\"/mnt/disk1\"]"],
            )
            .unwrap();
        w.db.mark_original("D/театр", MarkScope::EveryRoot).unwrap();

        // The array grows, the archive is read again, and nobody goes back to
        // the tree to say the same thing a second time.
        w.db.conn
            .execute(
                "UPDATE settings SET value = ?1 WHERE key = 'roots'",
                ["[\"/mnt/disk1\",\"/mnt/disk4\"]"],
            )
            .unwrap();
        let loose = file(&w, "/mnt/disk4/D/свалка", "IMG.JPG", 7);
        let good = file(&w, "/mnt/disk4/D/театр", "IMG.JPG", 7);
        let fam = family(&w, loose, &[loose, good]);

        assert_eq!(settle(&w.db).unwrap().moved, 1);
        assert_eq!(keeper_of(&w, fam), good);
    }

    #[test]
    fn a_folder_whose_name_merely_starts_the_same_is_not_covered() {
        let w = world();
        let elsewhere = file(&w, "/foto-old/2014", "DSC_0001.JPG", 1);
        let backup = file(&w, "/backup/2014", "DSC_0001.JPG", 1);
        let fam = family(&w, backup, &[backup, elsewhere]);

        w.db.mark_original("/foto", MarkScope::Absolute).unwrap();
        assert_eq!(settle(&w.db).unwrap(), Report::default());
        assert_eq!(keeper_of(&w, fam), backup);
    }

    #[test]
    fn a_frame_set_aside_by_hand_is_not_chosen_by_the_rule() {
        // The other half of the same promise. A rule may not pick a file the
        // person has rejected, and may not take the rejection back on its way
        // past: the group would then say two opposite things at once.
        let w = world();
        let backup = file(&w, "/backup/2014", "DSC_0001.JPG", 1);
        let original = file(&w, "/foto/2014", "DSC_0001.JPG", 1);
        let fam = family(&w, backup, &[backup, original]);
        w.db.conn
            .execute(
                "INSERT INTO manual_rejects(file_id, marked_at) VALUES(?1, 1)",
                [original],
            )
            .unwrap();

        w.db.mark_original("/foto", MarkScope::Absolute).unwrap();
        let report = settle(&w.db).unwrap();

        assert_eq!(report.groups, 1);
        assert_eq!(report.moved, 0, "правило взяло отклонённый файл");
        assert_eq!(keeper_of(&w, fam), backup);
        let still: i64 =
            w.db.conn
                .query_row(
                    "SELECT COUNT(*) FROM manual_rejects WHERE file_id = ?1",
                    [original],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(still, 1, "правило стёрло решение человека");
    }

    #[test]
    fn a_different_photograph_in_the_marked_folder_is_left_for_a_person() {
        let w = world();
        let backup = file(&w, "/backup/2014", "DSC_0001.JPG", 1);
        let export = file(&w, "/foto/2014", "DSC_0001.jpg", 9);
        let fam = family(&w, backup, &[backup, export]);

        w.db.mark_original("/foto", MarkScope::Absolute).unwrap();
        let report = settle(&w.db).unwrap();

        assert_eq!(
            report,
            Report {
                groups: 1,
                moved: 0,
                untouched: 1,
                by_hand: 0
            }
        );
        assert_eq!(keeper_of(&w, fam), backup, "кадр подменён на другой снимок");
    }

    #[test]
    fn a_keeper_the_user_chose_by_hand_outranks_the_rule() {
        let w = world();
        let backup = file(&w, "/backup/2014", "DSC_0001.JPG", 1);
        let original = file(&w, "/foto/2014", "DSC_0001.JPG", 1);
        let fam = family(&w, backup, &[backup, original]);
        w.db.set_manual_keeper(fam, backup).unwrap();

        w.db.mark_original("/foto", MarkScope::Absolute).unwrap();
        // The group is counted — the folder does hold a file of it, and that
        // is what "groups with a file here" means — and then left alone,
        // under its own heading. Reporting nothing at all made the rule look
        // as though it had missed the folder entirely.
        assert_eq!(
            settle(&w.db).unwrap(),
            Report {
                groups: 1,
                moved: 0,
                untouched: 0,
                by_hand: 1
            }
        );
        assert_eq!(keeper_of(&w, fam), backup);
    }

    #[test]
    fn a_rebuild_leaves_the_rule_and_its_plan_where_they_were() {
        // The worst failure this feature has had. Roles were worked out
        // against whichever file the measurement liked, the kept file was
        // swapped in afterwards, and nobody went back to fix the words: the
        // kept file was left labelled a copy of the one it had replaced, so
        // the copy was no longer a copy of anything and no plan would offer
        // it. A whole archive's worth of exact copies vanished from the plan
        // on the next rebuild, silently.
        let w = world();
        let store = pc_core::ThumbStore::new(w._tmp.path().join("thumbs"));
        let loose = file(&w, "/mnt/disk1/D/свалка", "IMG.JPG", 1);
        let good = file(&w, "/mnt/disk1/D/разобрано/театр", "IMG.JPG", 1);
        crate::build(&w.db, &store, &Default::default()).unwrap();

        w.db.mark_original("/mnt/disk1/D/разобрано/театр", MarkScope::Absolute)
            .unwrap();
        settle(&w.db).unwrap();
        let plan = |w: &World| {
            crate::plan::compute(&w.db, &Default::default())
                .unwrap()
                .candidates
                .len()
        };
        assert_eq!(plan(&w), 1, "отметка не дала плана");
        assert!(keeper_of_file(&w, good));

        crate::build(&w.db, &store, &Default::default()).unwrap();
        settle(&w.db).unwrap();
        assert_eq!(plan(&w), 1, "пересборка потеряла точную копию");
        assert!(keeper_of_file(&w, good), "пересборка сменила хранимый файл");
        let role: String =
            w.db.conn
                .query_row(
                    "SELECT role FROM family_members WHERE file_id = ?1",
                    [loose],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(role, "copy", "роли разошлись с хранимым файлом");
    }

    #[test]
    fn taking_the_mark_back_undoes_what_it_decided_and_nothing_else() {
        let w = world();
        let backup = file(&w, "/backup/2014", "DSC_0001.JPG", 1);
        let original = file(&w, "/foto/2014", "DSC_0001.JPG", 1);
        let fam = family(&w, backup, &[backup, original]);
        // The group's own idea of quality, so the fallback has something to
        // fall back to.
        w.db.conn
            .execute(
                "UPDATE family_members SET quality = 9 WHERE file_id = ?1",
                [backup],
            )
            .unwrap();

        let chosen = file(&w, "/backup/2015", "DSC_0002.JPG", 2);
        let other = file(&w, "/foto/2015", "DSC_0002.JPG", 2);
        let byhand = family(&w, other, &[chosen, other]);
        w.db.set_manual_keeper(byhand, chosen).unwrap();

        w.db.mark_original("/foto", MarkScope::Absolute).unwrap();
        settle(&w.db).unwrap();
        assert_eq!(keeper_of(&w, fam), original);

        w.db.unmark_original("/foto", MarkScope::Absolute).unwrap();
        assert_eq!(keeper_of(&w, fam), backup, "правило не откатилось");
        assert_eq!(keeper_of(&w, byhand), chosen, "снят выбор человека");
        assert!(w.db.original_marks().unwrap().is_empty());
    }

    #[test]
    fn marking_a_folder_inside_a_marked_one_leaves_one_rule() {
        let w = world();
        w.db.mark_original("/foto", MarkScope::Absolute).unwrap();
        w.db.mark_original("/foto/2014", MarkScope::Absolute)
            .unwrap();
        assert_eq!(paths(&w.db), ["/foto"]);

        w.db.mark_original("/archive", MarkScope::Absolute).unwrap();
        w.db.mark_original("/archive/raw", MarkScope::Absolute)
            .unwrap();
        assert_eq!(paths(&w.db), ["/archive", "/foto"]);
    }

    #[test]
    fn marking_a_folder_above_a_marked_one_absorbs_it() {
        let w = world();
        w.db.mark_original("/foto/2014", MarkScope::Absolute)
            .unwrap();
        w.db.mark_original("/foto/2015", MarkScope::Absolute)
            .unwrap();
        w.db.mark_original("/foto", MarkScope::Absolute).unwrap();
        assert_eq!(paths(&w.db), ["/foto"]);
    }
}
