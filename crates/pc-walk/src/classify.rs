use pc_core::DerivedKind;
use std::path::Path;

/// Recognise a derived-data bundle by directory name.
///
/// Returns the kind and, when the name implies one, the base name of the
/// owning Lightroom catalog (`"Dog Previews.lrdata"` -> `"Dog"`).
pub fn classify_dir(name: &str) -> Option<(DerivedKind, Option<String>)> {
    if let Some(stem) = name.strip_suffix(".lrdata") {
        // Order matters: "Smart Previews" also ends with "Previews".
        for (suffix, kind) in [
            (" Smart Previews", DerivedKind::LrSmartPreviews),
            (" Previews", DerivedKind::LrPreviews),
            (" Helper", DerivedKind::LrHelper),
        ] {
            if let Some(owner) = stem.strip_suffix(suffix) {
                if !owner.is_empty() {
                    return Some((kind, Some(owner.to_string())));
                }
            }
        }
        return Some((DerivedKind::LrDataOther, None));
    }

    if let Some(owner) = name.strip_suffix(".lrcat-data") {
        let owner = (!owner.is_empty()).then(|| owner.to_string());
        return Some((DerivedKind::LrCatalogData, owner));
    }

    if matches!(name, "@eaDir" | ".thumbnails") {
        return Some((DerivedKind::SystemJunk, None));
    }

    None
}

/// Catalogs under a backup or migration folder are user backups, not caches.
pub fn is_backup_path(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("Backups") | Some("Old Lightroom Catalogs")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_smart_previews_from_previews() {
        assert_eq!(
            classify_dir("Family Smart Previews.lrdata"),
            Some((DerivedKind::LrSmartPreviews, Some("Family".into())))
        );
        assert_eq!(
            classify_dir("Family Previews.lrdata"),
            Some((DerivedKind::LrPreviews, Some("Family".into())))
        );
    }

    #[test]
    fn handles_catalog_names_with_spaces() {
        assert_eq!(
            classify_dir("Lightroom Catalog Previews.lrdata"),
            Some((DerivedKind::LrPreviews, Some("Lightroom Catalog".into())))
        );
        assert_eq!(
            classify_dir("Dogshow_15.02.2025 Previews.lrdata"),
            Some((DerivedKind::LrPreviews, Some("Dogshow_15.02.2025".into())))
        );
    }

    #[test]
    fn catalog_data_is_recognised_and_is_not_regenerable() {
        let (kind, owner) = classify_dir("Тэфи_13.10.2025.lrcat-data").unwrap();
        assert_eq!(kind, DerivedKind::LrCatalogData);
        assert_eq!(owner.as_deref(), Some("Тэфи_13.10.2025"));
        assert!(!kind.regenerable());
    }

    #[test]
    fn helper_bundles_are_recognised() {
        assert_eq!(
            classify_dir("JustPhotos Helper.lrdata"),
            Some((DerivedKind::LrHelper, Some("JustPhotos".into())))
        );
    }

    #[test]
    fn ordinary_directories_are_not_bundles() {
        assert_eq!(classify_dir("foto"), None);
        assert_eq!(classify_dir("Lightroom_lib"), None);
    }

    #[test]
    fn backup_paths_are_detected() {
        assert!(is_backup_path(Path::new(
            "/mnt/disk2/foto/F/Lightroom Libraries/Work/Backups/2015-06-01 2138/Work.lrcat"
        )));
        assert!(is_backup_path(Path::new(
            "/mnt/disk3/foto/F/Lightroom Libraries/HDR/Old Lightroom Catalogs/HDR.lrcat"
        )));
        assert!(!is_backup_path(Path::new(
            "/mnt/disk3/foto/Lightroom_lib/JustPhotos/JustPhotos.lrcat"
        )));
    }
}
