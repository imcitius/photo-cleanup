//! Shared types and helpers.

pub mod bytes;
pub mod disk;
pub mod time;

pub use bytes::fmt_bytes;
pub use disk::{Disk, DiskMap};

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
            Self::LrPreviews => "Превью Lightroom",
            Self::LrSmartPreviews => "Smart Previews Lightroom",
            Self::LrHelper => "Helper-данные Lightroom",
            Self::LrDataOther => "Прочие бандлы .lrdata",
            Self::LrCatalogData => "Данные каталога Lightroom",
            Self::SystemJunk => "Системный мусор",
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
            Self::NotRegenerable => "регенерации нет, удаление необратимо".into(),
            Self::CatalogOpen => "каталог открыт в Lightroom".into(),
            Self::OriginalsMissing { missing, total } => {
                format!("оригиналы не найдены: {missing} из {total}")
            }
            Self::OwnerUnreadable { detail } => format!("каталог не прочитан: {detail}"),
        }
    }
}

/// True for names we never descend into or index.
pub fn is_system_junk_name(name: &str) -> bool {
    name == ".DS_Store" || name == "Thumbs.db" || name == "desktop.ini" || name.starts_with("._")
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
    )
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
