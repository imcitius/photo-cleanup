//! What derived data may be moved: nothing (el-126jk).
//!
//! One question, asked here so the scan, the command line, the web preview
//! and the move itself all get the same answer — and the answer is always a
//! reason to leave the object where it is:
//!
//! 1. **Nothing of Lightroom's is ever touched** — the user's decision of
//!    2026-10-06 — and nothing of a Photos library either (invariant 9).
//!    Catalogues, `.lrcat-data`, every `.lrdata` (previews, smart previews,
//!    helper), `.lrprev`, backups, `.photoslibrary` and its relatives, and
//!    anything inside any of them, at any depth.
//! 2. **No system file is moved either** — the contract the director narrowed
//!    after two independent reviews (el-wda81, el-2rpxq) found, twice, a file
//!    that a "is this junk?" check accepted and should not have:
//!    - a companion (`._X`, `X@SynoEAStream`, `X@SynoResource`) belongs to
//!      its file (invariant 8). Whether that file is there cannot be decided
//!      reliably — NFC/NFD, case, and what each filesystem folds differ — so
//!      a companion never moves on its own, with or without its file;
//!    - `.DS_Store`, `Thumbs.db`, `desktop.ini`, `@eaDir`, `.thumbnails`:
//!      a name proves nothing, and neither does a structure. Even a
//!      `.DS_Store` whose buddy allocator and B-tree are flawless carries
//!      arbitrary bytes in its `blob` records and free blocks, so no
//!      validation can show it holds nothing of a person's. A system file
//!      left behind costs one decision; a photograph moved by mistake costs
//!      the photograph.
//!
//! The walk no longer records any of these as derived data. A database
//! scanned by an earlier version still holds such rows; they are answered
//! here, from the kind and the path alone, without reading the disk.
//!
//! Matching is on `to_lowercase()` against ASCII patterns, so it holds for
//! `PREVIEWS.LRDATA` as much as for `Previews.lrdata`, and trailing dots and
//! spaces are dropped first — Windows and SMB resolve `Backups. ` to
//! `Backups`. Unicode normal forms need no folding of their own: canonical
//! (de)composition never produces or removes an ASCII letter, so an NFC and
//! an NFD spelling of the same name match an ASCII pattern identically.

use crate::{BlockReason, DerivedKind};
use std::path::Path;

/// A name as a share resolves it, for comparison: lower case, without the
/// trailing dots and spaces Windows and SMB ignore, without leading spaces.
pub fn fold_name(name: &str) -> String {
    name.trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_start()
        .to_lowercase()
}

/// Is this one path component something of Lightroom's?
///
/// Folders called `Backups` (or `Backup`) count whatever their parent:
/// Lightroom writes its catalogue backups there, and a person's own backups
/// are not something to clean junk out of either. A folder whose name
/// mentions Lightroom counts too. Both err towards leaving more than the
/// decision strictly names.
pub fn is_lightroom_name(name: &str) -> bool {
    let folded = fold_name(name);
    folded.contains("lightroom")
        || folded == "backups"
        || folded == "backup"
        || has_lightroom_extension(name)
}

/// Does the name carry one of Lightroom's own extensions? Such a folder is
/// an object of Lightroom's in itself — a walk records it and never enters.
///
/// Lightroom's names all carry an `.lr…` extension (`.lrcat`, `.lrcat-data`,
/// `.lrcat-wal`, `.lrcat.zip`, `.lrdata`, `.lrprev`, `.lrlibrary`,
/// `.lrtemplate`…), so any dot-separated part after the first that starts
/// with `lr` counts. Anything else with such an extension is kept too: a
/// file kept by mistake costs one decision, a catalogue lost costs years.
/// Purge asks the same question of what is already in quarantine.
pub fn has_lightroom_extension(name: &str) -> bool {
    name.to_lowercase()
        .split('.')
        .skip(1)
        .any(|ext| ext.trim_start().starts_with("lr"))
}

/// Why this one name is protected, if it is: Lightroom's, or a library
/// that owns its contents ([`crate::is_protected_bundle`]).
pub fn protected_name(name: &str) -> Option<BlockReason> {
    if is_lightroom_name(name) {
        return Some(BlockReason::Lightroom {
            part: name.to_string(),
        });
    }
    crate::is_protected_bundle(name).then(|| BlockReason::ProtectedLibrary {
        part: name.to_string(),
    })
}

fn names(path: &Path) -> impl Iterator<Item = std::borrow::Cow<'_, str>> {
    path.components().filter_map(|c| match c {
        std::path::Component::Normal(s) => Some(s.to_string_lossy()),
        _ => None,
    })
}

/// The first component of `path` that belongs to Lightroom, if any.
pub fn lightroom_part(path: &Path) -> Option<String> {
    names(path)
        .find(|s| is_lightroom_name(s))
        .map(|s| s.into_owned())
}

/// The first component of `path` that is protected, with the reason.
pub fn protected_part(path: &Path) -> Option<BlockReason> {
    names(path).find_map(|s| protected_name(&s))
}

/// Why this object stays where it is. There is always a reason: `derived
/// clean` moves nothing (see the module notes).
///
/// Lightroom and protected libraries are named first; then a companion, then
/// any other system file. Decided from the kind and the path alone — no byte
/// is read, so nothing on the disk can talk the answer round.
pub fn refusal(kind: DerivedKind, path: &Path) -> BlockReason {
    if kind.is_lightroom() {
        return BlockReason::Lightroom {
            part: lightroom_part(path).unwrap_or_else(|| kind.as_str().to_string()),
        };
    }
    if let Some(r) = protected_part(path) {
        return r;
    }
    let file = path.display().to_string();
    if is_companion_name(crate::file_name_str(path)) {
        BlockReason::Companion { file }
    } else {
        BlockReason::SystemFile { file }
    }
}

/// A companion of another file: AppleDouble `._X`, Synology's
/// `X@SynoEAStream` / `X@SynoResource` (or any other `@Syno…` stream).
pub fn is_companion_name(name: &str) -> bool {
    name.starts_with("._") || name.to_lowercase().contains("@syno")
}

/// What a preview of `derived clean` says when system junk is asked for,
/// as `(what, why)`: the scan records none of it any more, so without this
/// line a person would see an empty plan and no reason.
pub fn system_junk_note() -> (String, String) {
    (
        crate::tr!(
            "Системный мусор (.DS_Store, ._*, Thumbs.db, desktop.ini, @eaDir, .thumbnails)",
            "System junk (.DS_Store, ._*, Thumbs.db, desktop.ini, @eaDir, .thumbnails)"
        )
        .into(),
        crate::tr!(
            "derived clean не переносит системные файлы: ни имя, ни структура не доказывают, что в таком файле нет ничего вашего, а спутник (._*, @SynoEAStream, @SynoResource) принадлежит своему файлу. Скан их больше не записывает; оставленный файл стоит одного решения, перенесённая по ошибке фотография — фотографии.",
            "derived clean moves no system files: neither a name nor a structure proves such a file holds nothing of yours, and a companion (._*, @SynoEAStream, @SynoResource) belongs to its file. The scan no longer records them; a file left behind costs one decision, a photograph moved by mistake costs the photograph."
        )
        .into(),
    )
}

#[cfg(test)]
pub(crate) mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn every_lightroom_spelling_is_lightroom() {
        for name in [
            "Cat Previews.lrdata",
            "PREVIEWS.LRDATA",
            "Mixed Smart Previews.LrData",
            "X.lrcat-data",
            "X.LRCAT-DATA",
            "X.lrcat",
            "X.lrcat-wal",
            "X.lrcat.lock",
            "X.lrcat.zip",
            "p.lrprev",
            "Backups",
            "Lightroom Libraries",
            "Old Lightroom Catalogs",
            "._Cat.lrcat",
            // NFC "Ёлка" and NFD "Йод" (И + U+0306).
            "\u{401}\u{43b}\u{43a}\u{430} Previews.lrdata",
            "\u{418}\u{306}\u{43e}\u{434} Previews.lrdata",
        ] {
            assert!(is_lightroom_name(name), "{name}");
        }
        for name in [
            "foto",
            "lrdata",
            "IMG_0001.JPG",
            ".DS_Store",
            "@eaDir",
            "Lr",
        ] {
            assert!(!is_lightroom_name(name), "{name}");
        }
    }

    #[test]
    fn an_ancestor_makes_its_whole_tree_lightroom() {
        let p = Path::new("/a/Cat Previews.lrdata/sub/.DS_Store");
        assert_eq!(lightroom_part(p).as_deref(), Some("Cat Previews.lrdata"));
        let p = Path::new("/a/Backups/2026-01-01 1200/.DS_Store");
        assert_eq!(lightroom_part(p).as_deref(), Some("Backups"));
        assert_eq!(lightroom_part(Path::new("/a/foto/.DS_Store")), None);
    }

    /// B1: Lightroom's backup folder in the spellings a share hands back.
    #[test]
    fn reviewer_b1_every_backup_spelling_is_lightroom() {
        for name in [
            "Backups",
            "Backups. ",
            "Backups.",
            "Backups ",
            "BACKUPS",
            "backups",
        ] {
            assert!(is_lightroom_name(name), "{name:?}");
            let p = Path::new("/a").join(name).join(".DS_Store");
            assert!(lightroom_part(&p).is_some(), "{name:?}");
        }
    }

    #[test]
    fn lightroom_is_refused_by_kind_and_by_path() {
        let p = Path::new("/nowhere/Cat Previews.lrdata");
        for kind in [
            DerivedKind::LrPreviews,
            DerivedKind::LrSmartPreviews,
            DerivedKind::LrHelper,
            DerivedKind::LrDataOther,
            DerivedKind::LrCatalogData,
        ] {
            assert!(matches!(refusal(kind, p), BlockReason::Lightroom { .. }));
        }
        let junk = Path::new("/nowhere/Cat Previews.lrdata/.DS_Store");
        assert!(matches!(
            refusal(DerivedKind::SystemJunk, junk),
            BlockReason::Lightroom { .. }
        ));
        let lib = Path::new("/a/LIBRARY.PHOTOSLIBRARY. /x/.DS_Store");
        assert!(matches!(
            refusal(DerivedKind::SystemJunk, lib),
            BlockReason::ProtectedLibrary { .. }
        ));
    }

    /// el-2rpxq and the narrowed contract: a companion is never moved on its
    /// own, whether its file is there, gone, or spelled in another normal
    /// form — and the answer is the same with no file on the disk at all.
    #[test]
    fn a_companion_is_never_junk_with_or_without_its_file() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        fs::write(d.join("caf\u{e9}.png"), b"frame").unwrap();
        for name in [
            "._cafe\u{301}.png",
            "._caf\u{e9}.png",
            "._orphan",
            "._.DS_Store",
            "._Thumbs.db",
            "gone.png@SynoEAStream",
            "gone.png@SynoResource",
            "GONE.PNG@SYNOEASTREAM",
        ] {
            let p = d.join(name);
            fs::write(&p, fixtures::apple_double()).unwrap();
            assert!(
                matches!(
                    refusal(DerivedKind::SystemJunk, &p),
                    BlockReason::Companion { .. }
                ),
                "{name}"
            );
            let absent = Path::new("/nowhere").join(name);
            assert!(matches!(
                refusal(DerivedKind::SystemJunk, &absent),
                BlockReason::Companion { .. }
            ));
        }
    }

    /// el-wda81 B3, el-2rpxq B3-R2 and the narrowed contract: no system file
    /// is junk — malformed, truncated or well-formed alike, file or folder.
    #[test]
    fn no_system_file_is_junk_whatever_its_bytes() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let ds = fixtures::ds_store();
        let cfb = fixtures::thumbs_db();
        for (name, bytes) in [
            (".DS_Store", ds.clone()),
            (".DS_Store", fixtures::ds_store_impossible_node()),
            (".DS_Store", ds[..8].to_vec()),
            ("Thumbs.db", cfb.clone()),
            ("Thumbs.db", fixtures::thumbs_db_missing_mini_stream()),
            ("Thumbs.db", cfb[..8].to_vec()),
            ("desktop.ini", fixtures::desktop_ini_utf16()),
            ("desktop.ini", fixtures::desktop_ini_text()),
        ] {
            let p = d.join(name);
            fs::write(&p, &bytes).unwrap();
            assert!(
                matches!(
                    refusal(DerivedKind::SystemJunk, &p),
                    BlockReason::SystemFile { .. }
                ),
                "{name} {} bytes",
                bytes.len()
            );
        }
        for dir in ["@eaDir", ".thumbnails"] {
            let p = d.join(dir);
            fs::create_dir_all(&p).unwrap();
            assert!(matches!(
                refusal(DerivedKind::SystemJunk, &p),
                BlockReason::SystemFile { .. }
            ));
        }
        let why = refusal(DerivedKind::SystemJunk, &d.join(".DS_Store")).describe();
        assert!(why.contains("moves no system files"), "{why}");
    }
}
