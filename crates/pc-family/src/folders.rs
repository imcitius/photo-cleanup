//! Keeping what is left of a burst in one folder.
//!
//! A burst is often the same fifteen frames in two places: a working folder
//! and a backup of it, or a card dump and the copies someone made later. Each
//! frame is its own group of copies, and each group picks its keeper on its
//! own merits — so the burst can end up scattered, frame 1 kept here and
//! frame 2 kept there, with the sequence no longer sitting anywhere whole.
//!
//! This settles that. Between byte-identical copies the choice is arbitrary
//! anyway: whichever is kept, the bytes are the same. So it is made in favour
//! of the folder where the rest of the burst already lives.

use std::collections::HashMap;

use anyhow::Result;
use pc_db::Db;

struct Frame {
    file_id: i64,
    dir: String,
    family_id: i64,
    keeper_file: Option<i64>,
    pixel_hash: Option<Vec<u8>>,
}

/// Move each burst's keepers into the folder that holds most of the burst.
///
/// Only ever between copies with the same pixels: a keeper is never swapped
/// for a different photograph, and a group the user has decided by hand is
/// left exactly as they set it.
pub fn settle_keepers(db: &Db) -> Result<u64> {
    let manual: std::collections::HashSet<i64> = db
        .conn
        .prepare("SELECT file_id FROM manual_keepers")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut by_series: HashMap<i64, Vec<Frame>> = HashMap::new();
    {
        let mut st = db.conn.prepare(
            "SELECT sm.series_id, sm.file_id, f.path, fm.family_id, fa.keeper_file, f.pixel_hash
               FROM series_members sm
               JOIN files f ON f.id = sm.file_id
               JOIN family_members fm ON fm.file_id = sm.file_id
               JOIN families fa ON fa.id = fm.family_id
              WHERE f.state = 'present'",
        )?;
        let rows = st.query_map([], |r| {
            let path: String = r.get(2)?;
            Ok((
                r.get::<_, i64>(0)?,
                Frame {
                    file_id: r.get(1)?,
                    dir: pc_core::dir_name(&path).to_string(),
                    family_id: r.get(3)?,
                    keeper_file: r.get(4)?,
                    pixel_hash: r.get(5)?,
                },
            ))
        })?;
        for row in rows {
            let (series_id, frame) = row?;
            by_series.entry(series_id).or_default().push(frame);
        }
    }

    // What the keeper of each group looks like, so a swap can be refused when
    // the two files are not the same photograph after all.
    let mut hash_of: HashMap<i64, Option<Vec<u8>>> = HashMap::new();
    for frames in by_series.values() {
        for f in frames {
            hash_of.insert(f.file_id, f.pixel_hash.clone());
        }
    }

    let mut moved = 0u64;
    for frames in by_series.values() {
        // The folder holding most of the burst. Ties go to the first by name,
        // so the same archive settles the same way every time.
        let mut per_dir: HashMap<&str, usize> = HashMap::new();
        for f in frames {
            *per_dir.entry(f.dir.as_str()).or_default() += 1;
        }
        let Some(home) = per_dir
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(dir, _)| dir.to_string())
        else {
            continue;
        };

        for f in frames {
            if f.dir != home || Some(f.file_id) == f.keeper_file {
                continue;
            }
            let Some(keeper) = f.keeper_file else {
                continue;
            };
            if manual.contains(&keeper) || manual.contains(&f.file_id) {
                continue;
            }
            // Same pixels, or the keeper stays where it is: this pass moves
            // a decision between copies, never between photographs.
            let same = match (hash_of.get(&keeper), &f.pixel_hash) {
                (Some(Some(a)), Some(b)) => a == b,
                _ => false,
            };
            if !same {
                continue;
            }
            if db.set_family_keeper(f.family_id, f.file_id)? {
                moved += 1;
            }
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_db::files::NewFile;

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

    /// One frame in one folder. Two files with the same `pixels` are copies
    /// of each other, which is the only case this pass ever touches.
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
                phash: Some(1),
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
    fn a_burst_is_kept_in_the_folder_that_holds_most_of_it() {
        // Three frames in the working folder, two of them copied into a
        // backup. Left alone, the groups pick their keepers separately and
        // the burst ends up split between the two folders.
        let w = world();
        let (work, back) = ("/foto/2014", "/foto/backup/2014");
        let a1 = file(&w, work, "DSC_0188.JPG", 1);
        let a2 = file(&w, back, "DSC_0188.JPG", 1);
        let b1 = file(&w, work, "DSC_0189.JPG", 2);
        let b2 = file(&w, back, "DSC_0189.JPG", 2);
        let c1 = file(&w, work, "DSC_0190.JPG", 3);

        let fa = family(&w, a2, &[a1, a2]); // keeper in the backup
        let fb = family(&w, b2, &[b1, b2]); // keeper in the backup
        let fc = family(&w, c1, &[c1]);

        let s =
            w.db.insert_series("burst", None, None, None, false, w.run)
                .unwrap();
        for (i, f) in [a1, a2, b1, b2, c1].iter().enumerate() {
            w.db.insert_series_member(s, *f, i as i64, 1.0, "").unwrap();
        }

        assert_eq!(settle_keepers(&w.db).unwrap(), 2);
        assert_eq!(keeper_of(&w, fa), a1, "кадр 188 остался в бэкапе");
        assert_eq!(keeper_of(&w, fb), b1, "кадр 189 остался в бэкапе");
        assert_eq!(keeper_of(&w, fc), c1);
    }

    #[test]
    fn a_keeper_the_user_chose_is_never_moved() {
        let w = world();
        let (work, back) = ("/foto/2014", "/foto/backup/2014");
        let a1 = file(&w, work, "DSC_0188.JPG", 1);
        let a2 = file(&w, back, "DSC_0188.JPG", 1);
        let b1 = file(&w, work, "DSC_0189.JPG", 2);
        let fa = family(&w, a2, &[a1, a2]);
        family(&w, b1, &[b1]);
        w.db.conn
            .execute("INSERT INTO manual_keepers(file_id) VALUES (?1)", [a2])
            .unwrap();

        let s =
            w.db.insert_series("burst", None, None, None, false, w.run)
                .unwrap();
        for (i, f) in [a1, a2, b1].iter().enumerate() {
            w.db.insert_series_member(s, *f, i as i64, 1.0, "").unwrap();
        }

        assert_eq!(settle_keepers(&w.db).unwrap(), 0);
        assert_eq!(keeper_of(&w, fa), a2);
    }

    #[test]
    fn a_different_photograph_is_never_swapped_in() {
        // Same burst, same folder — but not the same pixels. Whatever the
        // folders say, one photograph is not a stand-in for another.
        let w = world();
        let (work, back) = ("/foto/2014", "/foto/backup/2014");
        let a1 = file(&w, work, "DSC_0188.JPG", 1);
        let a2 = file(&w, back, "DSC_0188.JPG", 9);
        let b1 = file(&w, work, "DSC_0189.JPG", 2);
        let fa = family(&w, a2, &[a1, a2]);
        family(&w, b1, &[b1]);

        let s =
            w.db.insert_series("burst", None, None, None, false, w.run)
                .unwrap();
        for (i, f) in [a1, a2, b1].iter().enumerate() {
            w.db.insert_series_member(s, *f, i as i64, 1.0, "").unwrap();
        }

        assert_eq!(settle_keepers(&w.db).unwrap(), 0);
        assert_eq!(keeper_of(&w, fa), a2);
    }
}
