//! Which file system object an operation moved — written down while it was
//! certainly ours, and asked again before anything is done on its behalf.
//!
//! A path says where something is supposed to be, not what is there. An undo
//! that found "a file named like the photograph" at home and called the
//! photograph back used to carry a sidecar beside a stranger's file and close
//! the row (el-usdqi, el-5vue3 R2/D4/D5). The journal now records, before the
//! first rename, what the object *is*: its device and inode, the kind of
//! entry, and for a file its size and modification time — which a rename
//! keeps and an unrelated file almost never shares — plus its birth time
//! where the platform keeps one.
//!
//! `dev:ino` alone is not proof across a restart: a deleted file's inode can
//! be handed out again. Size, modification time to the nanosecond and birth
//! time are what an inode handed to someone else does not bring along. That
//! is still evidence, not certainty: a file rewritten in place to the same
//! size with its timestamps forged, or an inode reused on a file system that
//! keeps no birth time with size and nanosecond mtime to match, is a residual
//! this cannot see. Where the platform cannot name an object at all
//! (Windows here, until it is verified natively), there is no proof, and the
//! callers refuse instead of guessing.

use serde::{Deserialize, Serialize};
use std::fs::Metadata;

/// The version of the evidence written now. Evidence of another version is
/// not understood, and what is not understood proves nothing.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Dir,
    Link,
    Other,
}

/// Read strictly: an unknown key or kind is evidence this version cannot
/// read, never evidence of a weaker kind (el-1y8uo B2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    pub v: u32,
    pub dev: u64,
    pub ino: u64,
    pub kind: Kind,
    /// Files only: a directory's size is not its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Files only: a rename keeps it; moving a directory may not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ns: Option<i128>,
    /// Where the platform reports one (APFS, ext4/xfs/btrfs through statx).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth_ns: Option<i128>,
    /// Content hash, where one was journaled. None is written today: reading
    /// every byte at the moment of a move is a cost this does not pay, and an
    /// index hash may be hours old.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
}

/// How an object found now compares with the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The same object, as far as the evidence reaches.
    Same,
    /// Another object, or this one changed: never ours to act on.
    Differs(&'static str),
    /// Nothing can be concluded: the evidence or the platform says too
    /// little. Treated exactly like `Differs` by every caller.
    Unprovable(&'static str),
}

fn nanos(t: std::io::Result<std::time::SystemTime>) -> Option<i128> {
    let t = t.ok()?;
    Some(match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    })
}

impl Proof {
    /// Evidence about the entry `md` describes (from `symlink_metadata`, or
    /// `fstat` of an open file). `None` where this platform cannot name an
    /// object.
    pub fn of(md: &Metadata) -> Option<Proof> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let ft = md.file_type();
            let kind = if ft.is_symlink() {
                Kind::Link
            } else if ft.is_dir() {
                Kind::Dir
            } else if ft.is_file() {
                Kind::File
            } else {
                Kind::Other
            };
            let file = kind == Kind::File;
            Some(Proof {
                v: VERSION,
                dev: md.dev(),
                ino: md.ino(),
                kind,
                size: file.then_some(md.len()),
                mtime_ns: file
                    .then(|| md.mtime() as i128 * 1_000_000_000 + md.mtime_nsec() as i128),
                birth_ns: nanos(md.created()),
                blake3: None,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = md;
            None
        }
    }

    /// Whether `md` is the object this evidence was taken from.
    /// The evidence against the object `md` describes, by metadata alone:
    /// [`Proof::verify`] with no content to hash. Evidence that records a
    /// hash is therefore never [`Verdict::Same`] here — ask
    /// [`Proof::check_file`] through an open descriptor instead.
    pub fn check(&self, md: &Metadata) -> Verdict {
        let Some(now) = Proof::of(md) else {
            return Verdict::Unprovable("this system cannot name a file system object");
        };
        self.verify(&now, None)
    }

    /// The evidence against the open `file`: its metadata and, where the
    /// evidence records a hash, its content read through that descriptor.
    pub fn check_file(&self, file: &std::fs::File) -> Verdict {
        let md = match file.metadata() {
            Ok(md) => md,
            Err(_) => return Verdict::Unprovable("it can no longer be read"),
        };
        let Some(now) = Proof::of(&md) else {
            return Verdict::Unprovable("this system cannot name a file system object");
        };
        self.verify(&now, Some(file))
    }

    /// The one comparison of recorded evidence with an object now (el-3s9kp,
    /// el-wffu8): every field the evidence records must match, and a field
    /// that cannot be read again is not a match. `now` is the object's
    /// metadata; `content` is the descriptor `now` was read through, needed
    /// only when the evidence records a hash, which is then recomputed
    /// whole. Every caller — move, rollback, recovery, purge of a file and of
    /// every file in a bundle — asks through this function; there is no
    /// caller-specific subset of fields.
    pub fn verify(&self, now: &Proof, content: Option<&std::fs::File>) -> Verdict {
        if self.v != VERSION {
            return Verdict::Unprovable("evidence of an unknown version");
        }
        if (now.dev, now.ino, now.kind) != (self.dev, self.ino, self.kind) {
            return Verdict::Differs("another object");
        }
        if self.size.is_some() && now.size != self.size {
            return Verdict::Differs("its size changed");
        }
        if self.mtime_ns.is_some() && now.mtime_ns != self.mtime_ns {
            return Verdict::Differs("it was modified");
        }
        match (self.birth_ns, now.birth_ns) {
            (Some(a), Some(b)) if a != b => return Verdict::Differs("another object (birth time)"),
            (Some(_), None) => return Verdict::Unprovable("its birth time can no longer be read"),
            _ => {}
        }
        let Some(want) = &self.blake3 else {
            return Verdict::Same;
        };
        let Some(file) = content else {
            return Verdict::Unprovable(
                "its recorded content hash is not checked without the file",
            );
        };
        // The descriptor must be the object `now` describes, or its bytes
        // prove nothing about it.
        match file.metadata().ok().as_ref().and_then(Proof::of) {
            Some(held) if (held.dev, held.ino) == (now.dev, now.ino) => {}
            _ => return Verdict::Unprovable("its content can no longer be read"),
        }
        match hash_of(file) {
            Some(got) if got.eq_ignore_ascii_case(want) => Verdict::Same,
            Some(_) => Verdict::Differs("its content changed"),
            None => Verdict::Unprovable("its content can no longer be read"),
        }
    }

    /// Short evidence for a refusal: what was checked, or what was found.
    pub fn shown(&self) -> String {
        format!(
            "dev {} inode {}, size {}, mtime {} ns",
            self.dev,
            self.ino,
            self.size.map_or("-".into(), |s| s.to_string()),
            self.mtime_ns.map_or("-".into(), |t| t.to_string())
        )
    }
}

/// BLAKE3 of the whole file, read by offset so the descriptor's position
/// (and whoever else reads through it) is not disturbed.
#[cfg(unix)]
fn hash_of(file: &std::fs::File) -> Option<String> {
    use std::os::unix::fs::FileExt;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut at = 0u64;
    loop {
        let n = file.read_at(&mut buf, at).ok()?;
        if n == 0 {
            return Some(h.finalize().to_hex().to_string());
        }
        h.update(&buf[..n]);
        at += n as u64;
    }
}

#[cfg(not(unix))]
fn hash_of(_: &std::fs::File) -> Option<String> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_rename_keeps_the_proof_and_a_rewrite_breaks_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, b"frame").unwrap();
        let p = Proof::of(&fs::symlink_metadata(&a).unwrap()).unwrap();
        fs::rename(&a, &b).unwrap();
        assert_eq!(p.check(&fs::symlink_metadata(&b).unwrap()), Verdict::Same);
        // Same inode, other content: what an inode handed out again looks
        // like to dev:ino.
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&b, b"other bytes").unwrap();
        assert!(matches!(
            p.check(&fs::symlink_metadata(&b).unwrap()),
            Verdict::Differs(_)
        ));
    }

    #[test]
    fn a_journaled_hash_is_compared_through_the_open_file() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        fs::write(&a, b"frame").unwrap();
        let f = fs::File::open(&a).unwrap();
        let mut p = Proof::of(&f.metadata().unwrap()).unwrap();
        assert_eq!(p.check_file(&f), Verdict::Same);
        p.blake3 = Some(blake3::hash(b"frame").to_hex().to_string());
        assert_eq!(p.check_file(&f), Verdict::Same);
        // Same metadata, other content on record: the hash decides.
        p.blake3 = Some(blake3::hash(b"other").to_hex().to_string());
        assert!(matches!(p.check_file(&f), Verdict::Differs(_)));
    }

    #[test]
    fn another_file_is_not_the_same_and_old_json_still_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, b"frame").unwrap();
        fs::write(&b, b"frame").unwrap();
        let p = Proof::of(&fs::symlink_metadata(&a).unwrap()).unwrap();
        assert!(matches!(
            p.check(&fs::symlink_metadata(&b).unwrap()),
            Verdict::Differs(_)
        ));
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Proof>(&json).unwrap(), p);
        let mut future = p.clone();
        future.v = VERSION + 1;
        assert!(matches!(
            future.check(&fs::symlink_metadata(&a).unwrap()),
            Verdict::Unprovable(_)
        ));
    }

    /// el-wffu8: the one comparison rejects each recorded field that
    /// differs on its own, and each recorded field it cannot read again.
    #[test]
    fn verify_rejects_every_single_differing_field() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        fs::write(&a, b"frame").unwrap();
        let f = fs::File::open(&a).unwrap();
        let now = Proof::of(&f.metadata().unwrap()).unwrap();
        let mut was = now.clone();
        was.birth_ns = Some(7);
        let mut now_b = now.clone();
        now_b.birth_ns = Some(7);
        was.blake3 = Some(blake3::hash(b"frame").to_hex().to_string());
        assert_eq!(was.verify(&now_b, Some(&f)), Verdict::Same);

        type Edit = fn(&mut Proof);
        let differs: [(&str, Edit); 7] = [
            ("dev", |p| p.dev += 1),
            ("ino", |p| p.ino += 1),
            ("kind", |p| p.kind = Kind::Dir),
            ("size", |p| p.size = p.size.map(|s| s + 1)),
            ("mtime", |p| p.mtime_ns = p.mtime_ns.map(|t| t + 1)),
            ("birth", |p| p.birth_ns = Some(8)),
            ("blake3", |p| {
                p.blake3 = Some(blake3::hash(b"other").to_hex().to_string())
            }),
        ];
        for (field, edit) in differs {
            let mut w = was.clone();
            edit(&mut w);
            assert!(
                matches!(w.verify(&now_b, Some(&f)), Verdict::Differs(_)),
                "{field}"
            );
        }

        // Recorded, and no longer readable: not a match.
        let mut gone = now_b.clone();
        gone.birth_ns = None;
        assert!(matches!(
            was.verify(&gone, Some(&f)),
            Verdict::Unprovable(_)
        ));
        assert!(matches!(was.verify(&now_b, None), Verdict::Unprovable(_)));
        assert!(matches!(
            was.check(&f.metadata().unwrap()),
            Verdict::Unprovable(_) | Verdict::Differs(_)
        ));
        let mut future = was.clone();
        future.v = VERSION + 1;
        assert!(matches!(
            future.verify(&now_b, Some(&f)),
            Verdict::Unprovable(_)
        ));

        // A descriptor of another object proves nothing about this one.
        let b = tmp.path().join("b");
        fs::write(&b, b"frame").unwrap();
        let other = fs::File::open(&b).unwrap();
        assert!(matches!(
            was.verify(&now_b, Some(&other)),
            Verdict::Unprovable(_)
        ));

        // Unrecorded fields are not invented: no hash, nothing to read.
        let mut plain = now.clone();
        plain.blake3 = None;
        assert_eq!(plain.verify(&now, None), Verdict::Same);
    }
}
