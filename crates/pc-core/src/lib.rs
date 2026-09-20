//! Shared types and helpers.

pub mod bytes;
pub mod disk;
pub mod lang;
pub mod thumbstore;
pub mod time;

pub use bytes::fmt_bytes;
pub use disk::{dev_of_nearest_existing, Disk, DiskMap};
pub use thumbstore::ThumbStore;

use std::path::Path;

/// Kinds of regenerable derived data the tool knows about.
///
/// The `regenerable` flag is a property of the kind itself, enforced in code
/// rather than policy: a non-regenerable kind can never be selected for
/// removal, from the UI or the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DerivedKind {
    /// `<Catalog> Previews.lrdata` — standard Lightroom preview pyramid.
    LrPreviews,
    /// `<Catalog> Smart Previews.lrdata` — lossy DNG proxies. Only regenerable
    /// while the originals they were built from are reachable.
    LrSmartPreviews,
    /// `<Catalog> Helper.lrdata` — small helper indices.
    LrHelper,
    /// Any other `*.lrdata` bundle we do not recognise.
    LrDataOther,
    /// `<Catalog>.lrcat-data` — AI masks, Denoise and part of the edit data.
    /// Not regenerable, never removable.
    LrCatalogData,
    /// macOS `.DS_Store`, AppleDouble `._*`, Windows `Thumbs.db`, `@eaDir`, ...
    SystemJunk,
}

impl DerivedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LrPreviews => "lr-previews",
            Self::LrSmartPreviews => "lr-smart-previews",
            Self::LrHelper => "lr-helper",
            Self::LrDataOther => "lr-lrdata-other",
            Self::LrCatalogData => "lr-catalog-data",
            Self::SystemJunk => "system-junk",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "lr-previews" => Self::LrPreviews,
            "lr-smart-previews" => Self::LrSmartPreviews,
            "lr-helper" => Self::LrHelper,
            "lr-lrdata-other" => Self::LrDataOther,
            "lr-catalog-data" => Self::LrCatalogData,
            "system-junk" => Self::SystemJunk,
            _ => return None,
        })
    }

    /// Whether the data can be rebuilt by its owning application.
    ///
    /// `LrSmartPreviews` is regenerable only conditionally; the condition is
    /// checked separately and recorded as a block reason.
    pub fn regenerable(self) -> bool {
        !matches!(self, Self::LrCatalogData)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LrPreviews => crate::tr!("Превью Lightroom", "Lightroom previews"),
            Self::LrSmartPreviews => "Lightroom Smart Previews",
            Self::LrHelper => crate::tr!("Helper-данные Lightroom", "Lightroom helper data"),
            Self::LrDataOther => crate::tr!("Прочие бандлы .lrdata", "Other .lrdata bundles"),
            Self::LrCatalogData => {
                crate::tr!("Данные каталога Lightroom", "Lightroom catalogue data")
            }
            Self::SystemJunk => crate::tr!("Системный мусор", "System junk"),
        }
    }
}

/// Why a bundle cannot be acted on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlockReason {
    /// The kind itself is not regenerable.
    NotRegenerable,
    /// A `.lrcat.lock` sits next to the owning catalog: Lightroom has it open.
    CatalogOpen,
    /// Smart previews whose originals are not all reachable.
    OriginalsMissing { missing: u64, total: u64 },
    /// The owning catalog could not be read, so the gate cannot be evaluated.
    OwnerUnreadable { detail: String },
}

impl BlockReason {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotRegenerable => "not-regenerable",
            Self::CatalogOpen => "catalog-open",
            Self::OriginalsMissing { .. } => "originals-missing",
            Self::OwnerUnreadable { .. } => "owner-unreadable",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::NotRegenerable => crate::tr!(
                "регенерации нет, удаление необратимо",
                "nothing regenerates this; removal is permanent"
            )
            .into(),
            Self::CatalogOpen => crate::tr!(
                "каталог открыт в Lightroom",
                "the catalogue is open in Lightroom"
            )
            .into(),
            Self::OriginalsMissing { missing, total } => crate::tf!(
                "оригиналы не найдены: {0} из {1}",
                "originals missing: {0} of {1}",
                missing,
                total
            ),
            Self::OwnerUnreadable { detail } => crate::tf!(
                "каталог не прочитан: {0}",
                "the catalogue could not be read: {0}",
                detail
            ),
        }
    }
}

/// True for names we never descend into or index.
pub fn is_system_junk_name(name: &str) -> bool {
    name == ".DS_Store" || name == "Thumbs.db" || name == "desktop.ini" || name.starts_with("._")
}

/// Application bundles that own their contents and must never be touched.
///
/// An Apple Photos library keeps its originals under UUID names and a SQLite
/// database that maps them; moving or deleting anything inside corrupts the
/// library, and a later "repair" can purge what it thinks are orphans. The
/// only correct way to delete from one is through Photos itself.
///
/// These are pruned outright for now. Indexing them read-only — so that loose
/// copies of photographs they already hold can be removed safely — is a
/// separate piece of work.
pub fn is_protected_bundle(name: &str) -> bool {
    const SUFFIXES: [&str; 5] = [
        ".photoslibrary",
        ".photolibrary",
        ".migratedphotolibrary",
        ".aplibrary",
        ".pvm",
    ];
    SUFFIXES.iter().any(|s| name.ends_with(s)) || name == "Photo Booth Library"
}

/// Directories that are pruned during the walk and never indexed.
pub fn is_pruned_dir_name(name: &str) -> bool {
    matches!(
        name,
        "@eaDir"
            | ".thumbnails"
            | ".Spotlight-V100"
            | ".fseventsd"
            | ".TemporaryItems"
            | ".Trashes"
            | ".Trash"
            | ".recycle"
            | "#recycle"
            | "node_modules"
            | ".git"
            | "Program Files"
            | "Windows"
            | "AppData"
    )
}

#[cfg(test)]
mod prune_tests {
    use super::*;

    #[test]
    fn photo_libraries_are_protected() {
        assert!(is_protected_bundle("Photos Library.photoslibrary"));
        assert!(is_protected_bundle("Old.migratedphotolibrary"));
        assert!(is_protected_bundle("Photo Booth Library"));
        assert!(!is_protected_bundle("Lightroom_lib"));
        assert!(!is_protected_bundle("foto"));
    }

    #[test]
    fn junk_names_are_recognised() {
        assert!(is_system_junk_name(".DS_Store"));
        assert!(is_system_junk_name("._DSC01234.ARW"));
        assert!(is_system_junk_name("Thumbs.db"));
        assert!(!is_system_junk_name("DSC01234.ARW"));
    }
}

/// Our own quarantine directory, so a rescan never re-reports quarantined data.
pub const QUARANTINE_DIR: &str = ".photo-cleanup-quarantine";

pub fn file_name_str(p: &Path) -> &str {
    p.file_name().and_then(|s| s.to_str()).unwrap_or("")
}

/// Russian plural agreement: 1 файл, 2 файла, 5 файлов.
pub fn plural_ru(n: i64, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let n = n.abs();
    if (11..=14).contains(&(n % 100)) {
        return many;
    }
    match n % 10 {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

/// `5 файлов` — count and correctly agreeing noun.
pub fn count_ru(n: i64, one: &'static str, few: &'static str, many: &'static str) -> String {
    format!("{n} {}", plural_ru(n, one, few, many))
}

/// A counted noun in whichever language is running.
///
/// Russian needs three forms and English two, so they are passed as arrays
/// rather than flattened into five arguments nobody could read at a glance.
pub fn count(n: i64, ru: [&'static str; 3], en: [&'static str; 2]) -> String {
    match lang::current() {
        lang::Lang::Ru => count_ru(n, ru[0], ru[1], ru[2]),
        lang::Lang::En => format!("{n} {}", if n == 1 { en[0] } else { en[1] }),
    }
}

#[cfg(test)]
mod plural_tests {
    use super::count_ru;

    #[test]
    fn agrees_in_russian() {
        let f = |n| count_ru(n, "файл", "файла", "файлов");
        assert_eq!(f(1), "1 файл");
        assert_eq!(f(2), "2 файла");
        assert_eq!(f(5), "5 файлов");
        assert_eq!(f(11), "11 файлов");
        assert_eq!(f(21), "21 файл");
        assert_eq!(f(114), "114 файлов");
        assert_eq!(f(0), "0 файлов");
    }
}

pub mod work;
