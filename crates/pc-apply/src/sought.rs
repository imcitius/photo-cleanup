//! Where a quarantined file went when its recorded place leads nowhere —
//! found by its evidence, and only reported (el-6byp2, diagnosis el-lvtmk
//! D1/D3/R8; director decision after review el-1oudi).
//!
//! The default quarantine sits inside the photo's own folder, so a user who
//! renames that folder after `apply` takes the quarantine along, and every
//! recorded path of the entry leads nowhere. Undo and reconciliation then
//! look for the file by what the journal recorded about it, and say where
//! it is and how that was proven. They never move it: the person returns it
//! by hand, and the entry stays open. A tool that moved what a search found
//! would have to prove, at the instant of the move, everything a person
//! sees at a glance — whose companion that `.xmp` beside it is, whether a
//! second name exists in a folder it could not read — and two rounds of
//! review showed it could not (el-14vx0, el-1oudi).
//!
//! The search, so that its answer is true:
//!
//! - *Bounded.* Only `.photo-cleanup-quarantine` folders under the roots of
//!   the entry's own run — the roots that hold the entry's recorded paths —
//!   and the recorded quarantine folder itself (the anchor). Never another
//!   run's roots, never the volume.
//! - *No links.* Each root is opened as the user named it (a link above the
//!   archive is the user's own path to it, as everywhere in this crate);
//!   below it, every folder is opened through the one above it, held open,
//!   without following a link (`O_NOFOLLOW`). A recorded anchor whose place
//!   now holds a link or anything but a folder is not followed, and the
//!   search says so.
//! - *Complete or uncertain.* A folder or entry that cannot be read, a name
//!   that cannot be spelled, a root that cannot be opened: the search is
//!   incomplete, and an incomplete search is never a unique answer.
//! - *Protected bundles are not entered* — a Photos library, Lightroom's own
//!   folders — and the search says which.
//! - *Proof.* A regular file bearing the recorded device and inode, opened
//!   without following a link, against the whole recorded evidence
//!   ([`Proof::check_file`]); and only evidence that records size and
//!   modification time — and birth time, where the system reports one —
//!   proves anything. Less is a candidate, never "found".

use anyhow::Result;
use pc_core::proof::{Kind, Proof};
use pc_db::{Db, JournalEntry, Moved, Sought, SoughtFinding, SoughtGap, SoughtHit};
use std::path::{Path, PathBuf};

pub(crate) fn quarantine_of(path: &Path) -> Option<PathBuf> {
    let mut at = PathBuf::new();
    let mut found = None;
    for c in path.components() {
        at.push(c);
        if c.as_os_str() == pc_core::QUARANTINE_DIR {
            found = Some(at.clone());
        }
    }
    found
}

/// A folder the search never enters: a library that owns its contents, or
/// anything of Lightroom's (`.lrdata`, `.lrcat-data`, `.lrlibrary`…).
#[cfg_attr(not(unix), allow(dead_code))]
fn protected(name: &str) -> bool {
    pc_core::is_protected_bundle(name) || pc_core::derived::has_lightroom_extension(name)
}

/// Evidence that can prove a file: size and modification time, and birth
/// time wherever the file found reports one. `None` when it can; otherwise
/// what is missing.
#[cfg_attr(not(unix), allow(dead_code))]
fn short_of(p: &Proof, birth_now: bool) -> Option<String> {
    if p.size.is_none() || p.mtime_ns.is_none() {
        return Some(
            pc_core::tr!(
                "запись хранит только устройство и inode — по ним одним файл не доказан",
                "the entry records only its device and inode — they alone prove nothing"
            )
            .to_string(),
        );
    }
    (p.birth_ns.is_none() && birth_now).then(|| {
        pc_core::tr!(
            "запись не хранит времени создания, а система его сообщает — доказательство неполное",
            "the entry records no birth time, which the system reports — the evidence is partial"
        )
        .to_string()
    })
}

/// What proved it, in words.
#[cfg_attr(not(unix), allow(dead_code))]
fn proven_by(p: &Proof) -> String {
    let mut parts = vec![pc_core::tr!(
        "устройство и inode, размер, время изменения",
        "device and inode, size, modification time"
    )];
    if p.birth_ns.is_some() {
        parts.push(pc_core::tr!("время создания", "birth time"));
    }
    if p.blake3.is_some() {
        parts.push(pc_core::tr!("хеш содержимого", "content hash"));
    }
    parts.join(", ")
}

/// The answer, from what was seen — in the order [`SoughtFinding`] gives.
fn finding(hits: &[SoughtHit], unread: &[SoughtGap], skipped: &[String]) -> SoughtFinding {
    if !unread.is_empty() {
        SoughtFinding::Uncertain
    } else if !skipped.is_empty() {
        SoughtFinding::ProtectedSkipped
    } else {
        match hits {
            [] => SoughtFinding::NotFound,
            [one] if one.proven => SoughtFinding::Found,
            [_] => SoughtFinding::Candidate,
            _ => SoughtFinding::Ambiguous,
        }
    }
}

/// Look for the object `look` records, `rec` being the manifest item, among
/// the quarantine folders of `entry`'s own run. Reads only.
pub(crate) fn seek(db: &Db, entry: &JournalEntry, rec: &Moved, look: &Moved) -> Result<Sought> {
    let mut s = Sought {
        src: rec.src.clone(),
        dst: rec.dst.clone(),
        scope: Vec::new(),
        finding: SoughtFinding::NoEvidence,
        hits: Vec::new(),
        unread: Vec::new(),
        skipped: Vec::new(),
    };
    let Some(p) = &look.proof else {
        return Ok(s);
    };
    if p.kind != Kind::File {
        s.unread.push(SoughtGap {
            at: rec.dst.clone(),
            why: pc_core::tr!(
                "по доказательству ищется только отдельный файл, а это не файл",
                "only a single file is looked for by its evidence, and this is not one"
            )
            .into(),
        });
        s.finding = finding(&s.hits, &s.unread, &s.skipped);
        return Ok(s);
    }
    let recorded = [rec.src.as_str(), rec.dst.as_str(), look.dst.as_str()];
    let (run, roots) = db.journal_run_roots(entry.id)?.unwrap_or_default();
    let roots: Vec<String> = roots
        .into_iter()
        .filter(|r| recorded.iter().any(|p| pc_core::under(p, r)))
        .collect();
    let mut anchors: Vec<String> = Vec::new();
    for d in [&rec.dst, &look.dst] {
        if let Some(q) = quarantine_of(Path::new(d)) {
            let q = q.display().to_string();
            if !anchors.contains(&q) {
                anchors.push(q);
            }
        }
    }
    search(&mut s, p, &roots, &anchors, run);
    s.finding = finding(&s.hits, &s.unread, &s.skipped);
    Ok(s)
}

#[cfg(unix)]
fn search(s: &mut Sought, p: &Proof, roots: &[String], anchors: &[String], run: i64) {
    let mut w = walk::Walk::new(p);
    if roots.is_empty() {
        w.gap(
            &s.src,
            pc_core::tf!(
                "ни один корень прогона {0} не содержит записанных путей — искать негде",
                "no root of run {0} holds the recorded paths — there is nowhere to look",
                run
            ),
        );
    }
    for r in roots {
        w.root(r);
    }
    for a in anchors {
        w.anchor(a, roots);
    }
    w.finish(s);
}

#[cfg(not(unix))]
fn search(s: &mut Sought, _: &Proof, _: &[String], _: &[String], _: i64) {
    s.unread.push(SoughtGap {
        at: s.dst.clone(),
        why: pc_core::tr!(
            "на этой системе поиск по доказательству не выполняется",
            "this system does not look for a file by its evidence"
        )
        .into(),
    });
}

#[cfg(unix)]
mod walk {
    use super::{protected, proven_by, short_of};
    use pc_core::anchored::{Dir, Ident};
    use pc_core::proof::{Proof, Verdict};
    use pc_db::{Sought, SoughtGap, SoughtHit};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    pub(super) struct Walk<'a> {
        proof: &'a Proof,
        want: Ident,
        trees: Vec<Dir>,
        tree_ids: BTreeSet<Ident>,
        hits: BTreeMap<(Ident, String), SoughtHit>,
        unread: BTreeMap<String, String>,
        skipped: BTreeSet<String>,
    }

    impl<'a> Walk<'a> {
        pub(super) fn new(proof: &'a Proof) -> Self {
            Walk {
                proof,
                want: (proof.dev, proof.ino),
                trees: Vec::new(),
                tree_ids: BTreeSet::new(),
                hits: BTreeMap::new(),
                unread: BTreeMap::new(),
                skipped: BTreeSet::new(),
            }
        }

        pub(super) fn gap(&mut self, at: impl std::fmt::Display, why: impl std::fmt::Display) {
            self.unread
                .entry(at.to_string())
                .or_insert_with(|| why.to_string());
        }

        fn add_tree(&mut self, t: Dir) {
            match t.ident() {
                Ok(id) => {
                    if self.tree_ids.insert(id) {
                        self.trees.push(t);
                    }
                }
                Err(e) => self.gap(t.path().display(), e),
            }
        }

        /// The names of `d`, each as a single entry name; what cannot be
        /// listed or spelled is a gap.
        fn names(&mut self, d: &Dir) -> Vec<String> {
            let names = match d.names() {
                Ok(n) => n,
                Err(e) => {
                    self.gap(d.path().display(), e);
                    return Vec::new();
                }
            };
            let mut out = Vec::new();
            for n in names {
                match n.to_str() {
                    Some(name) => out.push(name.to_string()),
                    None => self.gap(
                        d.path().join(&n).display(),
                        pc_core::tr!(
                            "имя не в UTF-8 — его нельзя открыть через удерживаемую папку",
                            "the name is not UTF-8 — it cannot be opened through its held folder"
                        ),
                    ),
                }
            }
            out
        }

        /// A folder below `d`, opened through it without following a link,
        /// and shown to be the entry `d` listed.
        fn child(&mut self, d: &Dir, name: &str, ident: Ident) -> Option<Dir> {
            match d.open_dir(name) {
                Ok(c) if c.ident().is_ok_and(|id| id == ident) => Some(c),
                Ok(_) => {
                    self.gap(
                        d.join(name).display(),
                        pc_core::tr!(
                            "папка сменилась, пока её читали",
                            "the folder changed while it was read"
                        ),
                    );
                    None
                }
                Err(e) => {
                    self.gap(d.join(name).display(), e);
                    None
                }
            }
        }

        /// Every quarantine folder under the run root `root`, on the
        /// object's device. The folders a scan never enters are not entered
        /// here either: apply never put a quarantine inside them.
        pub(super) fn root(&mut self, root: &str) {
            let top = match Dir::open_following(Path::new(root)) {
                Ok(d) => d,
                Err(e) => return self.gap(root, e),
            };
            let mut stack = vec![top];
            while let Some(d) = stack.pop() {
                for name in self.names(&d) {
                    let e = match d.entry_at(&name) {
                        Ok(e) => e,
                        Err(err) => {
                            self.gap(d.join(&name).display(), err);
                            continue;
                        }
                    };
                    if !e.is_dir() || e.ident.0 != self.want.0 {
                        continue;
                    }
                    if name == pc_core::QUARANTINE_DIR {
                        if let Some(t) = self.child(&d, &name, e.ident) {
                            self.add_tree(t);
                        }
                    } else if !protected(&name) && !pc_core::is_pruned_dir_name(&name) {
                        if let Some(c) = self.child(&d, &name, e.ident) {
                            stack.push(c);
                        }
                    }
                }
            }
        }

        /// The recorded quarantine folder `anchor`, reached from the run root
        /// that holds it one folder at a time, never through a link. Gone is
        /// an answer (its folder was renamed); a link, or anything but a
        /// folder, at its place is not followed and leaves the search
        /// incomplete.
        pub(super) fn anchor(&mut self, anchor: &str, roots: &[String]) {
            let Some((root, rel)) = roots
                .iter()
                .find_map(|r| pc_core::relative_to(anchor, r).map(|rel| (r, rel)))
            else {
                return self.gap(
                    anchor,
                    pc_core::tr!(
                        "записанная папка карантина лежит вне корней прогона записи — не читалась",
                        "the recorded quarantine folder is outside the roots of the entry's run — not read"
                    ),
                );
            };
            let mut d = match Dir::open_following(Path::new(root)) {
                Ok(d) => d,
                Err(e) => return self.gap(root, e),
            };
            for name in rel.split(['/', '\\']).filter(|c| !c.is_empty()) {
                if name == "." || name == ".." {
                    return self.gap(
                        anchor,
                        pc_core::tr!(
                            "в записанном пути есть «.» или «..» — не читалась",
                            "the recorded path holds “.” or “..” — not read"
                        ),
                    );
                }
                let e = match d.entry_at(name) {
                    Ok(e) => e,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                    Err(err) => return self.gap(d.join(name).display(), err),
                };
                if !e.is_dir() {
                    let link = e.mode & S_IFMT == S_IFLNK;
                    return self.gap(
                        d.join(name).display(),
                        if link {
                            pc_core::tr!(
                                "на месте записанной папки теперь ссылка — по ней не переходят",
                                "a link now stands where the recorded folder was — it is not followed"
                            )
                        } else {
                            pc_core::tr!(
                                "на месте записанной папки теперь не папка",
                                "something other than a folder now stands where the recorded folder was"
                            )
                        },
                    );
                }
                if protected(name) {
                    self.skipped.insert(d.join(name).display().to_string());
                    return;
                }
                if e.ident.0 != self.want.0 {
                    return self.gap(
                        d.join(name).display(),
                        pc_core::tr!(
                            "записанная папка теперь на другом устройстве — не читалась",
                            "the recorded folder is now on another device — not read"
                        ),
                    );
                }
                match self.child(&d, name, e.ident) {
                    Some(c) => d = c,
                    None => return,
                }
            }
            self.add_tree(d);
        }

        /// Read every quarantine folder found, and hand the answer over.
        pub(super) fn finish(mut self, s: &mut Sought) {
            let trees = std::mem::take(&mut self.trees);
            for t in &trees {
                s.scope.push(t.path().display().to_string());
            }
            for t in trees {
                self.tree(t);
            }
            s.hits = std::mem::take(&mut self.hits).into_values().collect();
            s.unread = std::mem::take(&mut self.unread)
                .into_iter()
                .map(|(at, why)| SoughtGap { at, why })
                .collect();
            s.skipped = std::mem::take(&mut self.skipped).into_iter().collect();
        }

        fn tree(&mut self, t: Dir) {
            let mut stack = vec![t];
            while let Some(d) = stack.pop() {
                for name in self.names(&d) {
                    let e = match d.entry_at(&name) {
                        Ok(e) => e,
                        Err(err) => {
                            self.gap(d.join(&name).display(), err);
                            continue;
                        }
                    };
                    if e.is_dir() {
                        if protected(&name) {
                            self.skipped.insert(d.join(&name).display().to_string());
                        } else if e.ident.0 == self.want.0 {
                            if let Some(c) = self.child(&d, &name, e.ident) {
                                stack.push(c);
                            }
                        }
                    } else if e.is_file() && e.ident == self.want {
                        self.hit(&d, &name);
                    }
                }
            }
        }

        fn hit(&mut self, d: &Dir, name: &str) {
            let Ok(dir_id) = d.ident() else {
                return self.gap(
                    d.path().display(),
                    pc_core::tr!("папку нельзя прочитать", "the folder cannot be read"),
                );
            };
            let key = (dir_id, name.to_string());
            if self.hits.contains_key(&key) {
                return;
            }
            let (proven, how) = self.prove(d, name);
            let hit = SoughtHit {
                at: d.join(name).display().to_string(),
                proven,
                how,
                beside: beside(d, name),
            };
            self.hits.insert(key, hit);
        }

        fn prove(&self, d: &Dir, name: &str) -> (bool, String) {
            let f = match d.open_file(name, false) {
                Ok(f) => f,
                Err(e) => {
                    return (
                        false,
                        pc_core::tf!("его нельзя открыть: {0}", "it cannot be opened: {0}", e),
                    )
                }
            };
            match self.proof.check_file(&f) {
                Verdict::Same => {}
                Verdict::Differs(why) | Verdict::Unprovable(why) => {
                    return (false, why.to_string())
                }
            }
            let held = f.metadata();
            let still = d.entry_at(name).is_ok_and(|e| e.ident == self.want);
            match held {
                Ok(md) if still => match short_of(self.proof, md.created().is_ok()) {
                    None => (true, proven_by(self.proof)),
                    Some(why) => (false, why),
                },
                _ => (
                    false,
                    pc_core::tr!(
                        "имя больше не ведёт к проверенному файлу",
                        "the name no longer leads to the file checked"
                    )
                    .to_string(),
                ),
            }
        }
    }

    /// Names a companion of `name` would bear in `d`, of whatever there now
    /// (a link, a dangling one included, or an entry that cannot be read):
    /// none of it is proven to be the entry's, all of it is for a person to
    /// look at before returning the file.
    fn beside(d: &Dir, name: &str) -> Vec<String> {
        let stem = name.rsplit_once('.').map_or(name, |(a, _)| a);
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for c in [
            format!("{stem}.xmp"),
            format!("{stem}.XMP"),
            format!("{name}.xmp"),
            format!("{stem}.aae"),
            format!("{stem}.AAE"),
            format!("{name}.pp3"),
            format!("._{name}"),
        ] {
            if c == name {
                continue;
            }
            match d.entry_at(&c) {
                // On a case-insensitive system `.xmp` and `.XMP` can be one
                // entry: named once.
                Ok(e) if !seen.insert(e.ident) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                _ => out.push(d.join(&c).display().to_string()),
            }
        }
        out
    }

    /// POSIX file-type bits of `st_mode` (the same on every Unix this
    /// builds for); `pc-apply` takes no `libc` of its own for two numbers.
    const S_IFMT: u32 = 0o170000;
    const S_IFLNK: u32 = 0o120000;
}

/// One search, in words: where the file is and how that was proven, or why
/// nothing is claimed. Shown alike by the command line and the web.
pub(crate) fn shown(s: &Sought) -> String {
    let named = |hits: &[SoughtHit]| -> String {
        if hits.is_empty() {
            return "-".into();
        }
        hits.iter()
            .map(|h| {
                if h.proven {
                    pc_core::tf!("{0} (доказан: {1})", "{0} (proven: {1})", h.at, h.how)
                } else {
                    pc_core::tf!(
                        "{0} (не доказан: {1})",
                        "{0} (not proven: {1})",
                        h.at,
                        h.how
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let head = pc_core::tf!(
        "{0} — записанное место {1} никуда не ведёт",
        "{0} — its recorded place {1} leads nowhere",
        s.src,
        s.dst
    );
    let scope = if s.scope.is_empty() {
        "-".to_string()
    } else {
        s.scope.join(", ")
    };
    let mut out = match s.finding {
        SoughtFinding::Found => pc_core::tf!(
            "{0}; найден по доказательству в {1} (доказан: {2})",
            "{0}; found by its evidence at {1} (proven: {2})",
            head,
            s.hits.first().map_or("-", |h| h.at.as_str()),
            s.hits.first().map_or("-", |h| h.how.as_str())
        ),
        SoughtFinding::Candidate => pc_core::tf!(
            "{0}; кандидат, не доказан: {1}",
            "{0}; a candidate, not proven: {1}",
            head,
            named(&s.hits)
        ),
        SoughtFinding::Ambiguous => pc_core::tf!(
            "{0}; к нему ведёт больше одного имени: {1}",
            "{0}; more than one name leads to it: {1}",
            head,
            named(&s.hits)
        ),
        SoughtFinding::NotFound => pc_core::tf!(
            "{0}; в папках карантина ({1}) по доказательству не найден",
            "{0}; not found by its evidence in the quarantine folders ({1})",
            head,
            scope
        ),
        SoughtFinding::Uncertain => pc_core::tf!(
            "{0}; поиск неполный — найденное ничего не утверждает: {1}",
            "{0}; the search is incomplete — nothing it saw is a unique answer: {1}",
            head,
            named(&s.hits)
        ),
        SoughtFinding::ProtectedSkipped => pc_core::tf!(
            "{0}; поиск не заходил в защищённые пакеты — найденное ничего не утверждает: {1}",
            "{0}; the search did not enter protected bundles — nothing it saw is a unique answer: {1}",
            head,
            named(&s.hits)
        ),
        SoughtFinding::NoEvidence => pc_core::tf!(
            "{0}, а запись не хранит доказательства: по имени его не ищут",
            "{0}, and the entry records no evidence: it is not looked for by name",
            head
        ),
    };
    if !s.unread.is_empty() {
        let gaps = s
            .unread
            .iter()
            .map(|g| format!("{} ({})", g.at, g.why))
            .collect::<Vec<_>>()
            .join(", ");
        out = pc_core::tf!("{0}; не прочитано: {1}", "{0}; not read: {1}", out, gaps);
    }
    if !s.skipped.is_empty() {
        out = pc_core::tf!(
            "{0}; не входил в: {1}",
            "{0}; not entered: {1}",
            out,
            s.skipped.join(", ")
        );
    }
    for h in s.hits.iter().filter(|h| !h.beside.is_empty()) {
        out = pc_core::tf!(
            "{0}; рядом с {1}: {2} — возможные спутники, не доказано, что они этой записи; \
             верните их вместе с кадром, если они его",
            "{0}; beside {1}: {2} — possible companions, not proven to be this entry's; \
             return them with the frame if they are its own",
            out,
            h.at,
            h.beside.join(", ")
        );
    }
    pc_core::tf!(
        "{0}; инструмент такой файл не переносит — верните вручную, запись остаётся открытой",
        "{0}; the tool does not move such a file — return it by hand, the entry stays open",
        out
    )
}

/// An undo or reconciliation that searched by evidence and moved nothing:
/// every search, as recorded in the entry's history, and the words for it.
#[derive(Debug, Clone)]
pub struct Unrecovered {
    pub sought: Vec<Sought>,
    pub text: String,
}

impl std::fmt::Display for Unrecovered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for Unrecovered {}

pub fn unrecovered(e: &anyhow::Error) -> Option<&Unrecovered> {
    e.downcast_ref::<Unrecovered>()
}
