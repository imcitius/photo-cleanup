//! Shared types and helpers.

pub mod bytes;
pub mod disk;
pub mod lang;
pub mod lock;
pub mod thumbstore;
pub mod time;
pub mod volume;

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

/// Separators that divide the components of a stored path.
///
/// Platform-dependent on purpose. Windows accepts both and writes the
/// backslash, so both have to count there. On Unix a backslash is an ordinary
/// character in a file name — `a\b.jpg` is one file, not a file `b.jpg` in a
/// directory `a` — and treating it as a separator would silently mis-name
/// entries the archive legitimately contains.
const SEPARATORS: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };

/// Split a stored path into its directory and its file name.
///
/// Paths become strings once they are in the database, so this is string work
/// rather than `Path` work, and it has to know which separators count.
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind(SEPARATORS) {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// The last component of a stored path.
pub fn base_name(path: &str) -> &str {
    split_path(path).1
}

/// The directory a stored path sits in.
pub fn dir_name(path: &str) -> &str {
    split_path(path).0
}

/// Are two path components the same component?
///
/// On Windows a path can come back in more than one spelling — the index
/// writes what the operating system gave it, a root can be typed by hand with
/// forward slashes, and the same directory answers to either case. Comparing
/// the raw bytes would then say that a folder is not inside the root it is
/// plainly inside of.
fn same_component(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// What is left of `path` below the directory `dir`, or `None` when `path`
/// does not lie there at all.
///
/// Compared component by component rather than by raw prefix, for two
/// reasons. A prefix match alone answers the wrong question — `/foto/2014-old`
/// starts with `/foto/2014` and is a different folder — and on Windows the
/// same directory is spelled several ways, so the bytes of the two strings
/// disagree where the folders do not.
///
/// An empty `dir` means "no folder given", and the whole path is what lies
/// below it: a path relative to nothing is itself. That is deliberate, and it
/// is what an archive with no roots configured is read against — the paths
/// stay absolute, leading separator and all.
pub fn relative_to<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    // Almost always the two are spelled the same way — both came off the same
    // filesystem — and then this is one comparison instead of one per folder.
    // The archive asks this question once per file per root, sixty thousand
    // times over, so the common case is worth having.
    let plain = trim_trailing_separators(dir);
    if plain.is_empty() {
        return Some(path);
    }
    if let Some(rest) = path.strip_prefix(plain) {
        if rest.is_empty() || rest.starts_with(SEPARATORS) {
            return Some(trim_leading_separators(rest));
        }
    }
    let mut rest = path;
    let mut named = false;
    for wanted in dir.split(SEPARATORS).filter(|c| !c.is_empty()) {
        named = true;
        rest = trim_leading_separators(rest);
        let end = rest.find(SEPARATORS).unwrap_or(rest.len());
        if !same_component(&rest[..end], wanted) {
            return None;
        }
        rest = &rest[end..];
    }
    // A folder whose name merely starts the same way is a different folder,
    // and the walk above has already refused it: what is left here can only
    // be empty or begin at a separator.
    Some(if named {
        trim_leading_separators(rest)
    } else {
        path
    })
}

/// True when `path` is the directory `dir` or lies anywhere beneath it.
pub fn under(path: &str, dir: &str) -> bool {
    relative_to(path, dir).is_some()
}

/// A root and a path below it, joined the way this platform writes paths.
///
/// Not `format!("{root}/{rel}")`: on Windows that produces `C:\\archive/D`,
/// which no file in the index is ever called, and an absolute mark built from
/// it would match nothing at all.
pub fn join_path(root: &str, rel: &str) -> String {
    let root = trim_trailing_separators(root);
    // No root means the path below it is already the whole path, leading
    // separator and all — `relative_to` hands it back untouched, and putting
    // it together again must not quietly make it relative.
    if root.is_empty() {
        return rel.to_string();
    }
    let rel = trim_leading_separators(rel);
    if rel.is_empty() {
        return root.to_string();
    }
    format!("{root}{}{rel}", std::path::MAIN_SEPARATOR)
}

/// A path with its leading separators removed, as after stripping a root off
/// the front of one. Platform-aware for the same reason as `split_path`.
pub fn trim_leading_separators(path: &str) -> &str {
    path.trim_start_matches(SEPARATORS)
}

/// A path with its trailing separators removed, so that a folder written by
/// hand as `/foto/` and one read from the index as `/foto` are one folder.
pub fn trim_trailing_separators(path: &str) -> &str {
    path.trim_end_matches(SEPARATORS)
}

/// Path components, with empty ones dropped.
pub fn path_parts(path: &str) -> Vec<&str> {
    path.split(SEPARATORS).filter(|s| !s.is_empty()).collect()
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
mod path_tests {
    use super::*;

    #[test]
    fn a_stored_path_splits_at_its_last_separator() {
        assert_eq!(split_path("/foto/2019/a.jpg"), ("/foto/2019", "a.jpg"));
        assert_eq!(split_path("a.jpg"), ("", "a.jpg"));
        assert_eq!(path_parts("/mnt/disk3/foto/"), ["mnt", "disk3", "foto"]);
    }

    #[test]
    fn a_folder_contains_itself_and_what_lies_below_it() {
        assert!(under("/foto/2014", "/foto/2014"));
        assert!(under("/foto/2014/DSC_0001.JPG", "/foto/2014"));
        assert!(under("/foto/2014/raw/DSC_0001.NEF", "/foto/2014"));
        assert!(under("/foto/2014/DSC_0001.JPG", "/foto/2014/"));
        assert!(under("/foto/2014/DSC_0001.JPG", "/"));
    }

    #[test]
    fn what_lies_below_a_folder_is_the_path_without_it() {
        assert_eq!(relative_to("/foto/2014/a.jpg", "/foto"), Some("2014/a.jpg"));
        assert_eq!(relative_to("/foto/2014", "/foto/2014"), Some(""));
        assert_eq!(
            relative_to("/foto/2014/a.jpg", "/foto/"),
            Some("2014/a.jpg")
        );
        assert_eq!(relative_to("/foto/2015/a.jpg", "/foto/2014"), None);
    }

    #[test]
    fn a_path_relative_to_nothing_is_itself() {
        // An archive with no roots configured is read against this, and the
        // leading separator has to survive: the tree and the marks both build
        // absolute paths back out of what this returns.
        assert_eq!(relative_to("/disk/z/a.jpg", ""), Some("/disk/z/a.jpg"));
        assert_eq!(join_path("", "/disk/z"), "/disk/z");
    }

    #[test]
    fn a_root_and_a_folder_join_the_way_the_platform_writes_paths() {
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            join_path("/mnt/disk1", "D/театр"),
            format!("/mnt/disk1{sep}D/театр")
        );
        assert_eq!(join_path("/mnt/disk1/", "/D"), format!("/mnt/disk1{sep}D"));
        assert_eq!(join_path("/mnt/disk1", ""), "/mnt/disk1");
    }

    #[test]
    fn a_name_that_merely_starts_the_same_is_a_different_folder() {
        assert!(!under("/foto/2014-old/DSC_0001.JPG", "/foto/2014"));
        assert!(!under("/foto/2015/DSC_0001.JPG", "/foto/2014"));
        assert!(!under("/foto", "/foto/2014"));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_backslash_in_a_unix_name_stays_part_of_the_name() {
        // Legal on every Unix filesystem. Counting it as a separator would
        // show the wrong name for a file the archive really contains.
        assert_eq!(
            split_path(r"/foto/2019/a\b.jpg"),
            ("/foto/2019", r"a\b.jpg")
        );
        assert_eq!(path_parts(r"/foto/a\b"), ["foto", r"a\b"]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_accepts_both_separators() {
        assert_eq!(
            split_path(r"D:\Фото\2019\a.jpg"),
            (r"D:\Фото\2019", "a.jpg")
        );
        assert_eq!(split_path("D:/Фото/a.jpg"), ("D:/Фото", "a.jpg"));
        assert_eq!(path_parts(r"D:\foto\2019"), ["D:", "foto", "2019"]);
    }
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

    #[test]
    fn a_way_out_of_quarantine_keeps_the_shape_of_what_went_in() {
        let q = QUARANTINE_DIR;
        assert_eq!(
            quarantine_origin(&format!("/foto/{q}/a.jpg")).unwrap(),
            format!("/foto{}a.jpg", std::path::MAIN_SEPARATOR)
        );
        // A directory went in whole, so everything under it comes back whole.
        // The head is quoted as it was stored; only the joints are ours.
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            quarantine_origin(&format!("/foto/{q}/Library.lrdata/sub/cache")).unwrap(),
            format!("/foto{sep}Library.lrdata{sep}sub{sep}cache")
        );
        // Not in quarantine, or nothing after the folder: no guessing.
        assert_eq!(quarantine_origin("/foto/a.jpg"), None);
        assert_eq!(quarantine_origin(&format!("/foto/{q}")), None);
        assert_eq!(quarantine_origin(&format!("/foto/{q}/")), None);
        // Quarantine inside quarantine: the way out is the innermost one.
        assert_eq!(
            quarantine_origin(&format!("/foto/{q}/dir/{q}/a.jpg")).unwrap(),
            format!("/foto/{q}/dir{sep}a.jpg")
        );
    }
}

/// Our own quarantine directory, so a rescan never re-reports quarantined data.
pub const QUARANTINE_DIR: &str = ".photo-cleanup-quarantine";

/// The file a gathered quarantine keeps beside its contents, saying where
/// those contents came from.
///
/// A quarantine beside each file needs nothing written down: the way out is
/// the folder above the hidden one, and the path itself carries it. A
/// gathered one does not — it holds `<label>/<path from that disk's mount>`,
/// and the label is a short name like `disk3` that means nothing without the
/// mount point it stood for. Guessing was how a file came home to
/// `collected/root/<its own old absolute path>`: bytes intact, address
/// invented.
///
/// So the layout says what it is, next to the data, and a database is not
/// needed to read it back.
pub const QUARANTINE_LAYOUT: &str = "where-these-came-from.json";

/// Where a path inside a quarantine folder came from.
///
/// Quarantine is a hidden folder beside the file, and what went in kept the
/// shape it had: a directory moved whole still has its own tree under there.
/// So the way home is not "one level up" — it is everything after the
/// quarantine component, hung back on the directory that holds it.
///
/// `.../foto/.photo-cleanup-quarantine/Library.lrdata/sub/cache`
/// comes home to `.../foto/Library.lrdata/sub/cache`.
///
/// `None` when the path is not inside a quarantine folder at all: nothing
/// should be moved on a guess.
/// Where a quarantined file came from, reading a gathered quarantine's note
/// when the path turns out to be in one.
///
/// A quarantine beside each file carries its own answer in the path. A
/// gathered one carries a disk label instead, which stands for a mount point
/// only its note remembers — so that is read, and when it says nothing the
/// answer is nothing. A path invented from a label is a file brought home to
/// an address that never existed.
pub fn quarantine_origin_of(path: &str) -> Option<String> {
    let inside = quarantine_inside(path)?;
    let home = std::path::Path::new(&path[..inside.0]).join(QUARANTINE_DIR);
    let disks = quarantine_layout::read(&home);
    if disks.is_empty() {
        return quarantine_origin(path);
    }
    let rest = trim_leading_separators(&path[inside.1..]);
    quarantine_layout::origin(&disks, std::path::Path::new(rest)).map(|p| p.display().to_string())
}

/// The byte offsets around the quarantine component of a path: where the part
/// above it ends, and where the part below it starts.
fn quarantine_inside(path: &str) -> Option<(usize, usize)> {
    let mut found = None;
    let mut start = 0;
    for (i, ch) in path.char_indices() {
        if SEPARATORS.contains(&ch) {
            if &path[start..i] == QUARANTINE_DIR {
                found = Some((start, i + ch.len_utf8()));
            }
            start = i + ch.len_utf8();
        }
    }
    found
}

pub fn quarantine_origin(path: &str) -> Option<String> {
    // The last quarantine component wins: quarantine inside quarantine is
    // still a path whose way out is the innermost folder. Working in byte
    // offsets keeps the tail spelled as it was stored, separators and all.
    let mut found = None;
    let mut start = 0;
    for (i, ch) in path.char_indices() {
        if SEPARATORS.contains(&ch) {
            if &path[start..i] == QUARANTINE_DIR {
                found = Some((start, i + ch.len_utf8()));
            }
            start = i + ch.len_utf8();
        }
    }
    // A trailing component is the folder itself, with nothing inside it to
    // bring home.
    let (head_end, tail_start) = found?;
    // The head keeps the spelling it was stored with — a drive letter, a
    // verbatim prefix, whatever the walk wrote. The tail is plain names, so
    // it is rejoined with this platform's separator rather than left mixed.
    let rest = trim_leading_separators(&path[tail_start..]);
    if rest.is_empty() {
        return None;
    }
    let tail = path_parts(rest).join(std::path::MAIN_SEPARATOR_STR);
    let head = path[..head_end].trim_end_matches(SEPARATORS);
    let sep = std::path::MAIN_SEPARATOR;
    // An absolute path keeps its leading separator; a relative one has none.
    Some(if head.is_empty() && path.starts_with(SEPARATORS) {
        format!("{sep}{tail}")
    } else {
        format!("{head}{sep}{tail}")
    })
}

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

/// `1 file` / `5 files` — for output that is English regardless, such as the
/// command line.
pub fn count_en(n: i64, one: &'static str, many: &'static str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
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

/// What a gathered quarantine says about itself.
///
/// Read from `QUARANTINE_LAYOUT` beside the data. Missing or unreadable means
/// "this is not a gathered quarantine, or nothing recorded it" — and then the
/// way home is not guessed.
pub mod quarantine_layout {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// Where each disk label the gathered quarantine uses was mounted.
    pub type Disks = BTreeMap<String, String>;

    fn file(root: &Path) -> PathBuf {
        root.join(super::QUARANTINE_LAYOUT)
    }

    pub fn read(root: &Path) -> Disks {
        std::fs::read_to_string(file(root))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Record that this label stood for this mount point.
    ///
    /// Written before the first file of a disk lands and left alone
    /// afterwards, so the note is there for anything that arrives later — and
    /// so a reader finds it whatever order the moves happened in.
    pub fn note(root: &Path, label: &str, mount: &Path) -> std::io::Result<()> {
        let mut disks = read(root);
        let mount = mount.display().to_string();
        if disks.get(label).map(String::as_str) == Some(mount.as_str()) {
            return Ok(());
        }
        disks.insert(label.to_string(), mount);
        std::fs::create_dir_all(root)?;
        let body = serde_json::to_string_pretty(&disks)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(file(root), body)
    }

    /// The path a file under a gathered quarantine came from.
    ///
    /// `rest` is what lies below the gathered root: `<label>/<path from that
    /// disk's mount>`. Without a note for that label there is no answer, and
    /// inventing one is worse than saying so.
    pub fn origin(disks: &Disks, rest: &Path) -> Option<PathBuf> {
        let mut parts = rest.components();
        let label = parts.next()?.as_os_str().to_str()?;
        let mount = disks.get(label)?;
        let tail = parts.as_path();
        if tail.as_os_str().is_empty() {
            return None;
        }
        Some(Path::new(mount).join(tail))
    }
}
