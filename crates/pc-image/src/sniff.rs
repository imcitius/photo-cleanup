//! Container detection from magic bytes.
//!
//! The extension is a hint for read ordering and nothing more: a quarter of
//! this archive's images carry no extension at all, and plenty of the rest
//! carry the wrong one.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Container {
    Jpeg,
    Png,
    /// TIFF byte order, which also covers ARW, CR2, NEF, DNG and friends.
    Tiff,
    Bmp,
    WebP,
    Gif,
    Psd,
    /// ISO-BMFF with a HEIC/AVIF brand.
    Heif,
    Unknown,
}

impl Container {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
            Self::WebP => "webp",
            Self::Gif => "gif",
            Self::Psd => "psd",
            Self::Heif => "heif",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_image(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// TIFF containers keep an index of their contents, so a large preview can
    /// be lifted out with a seek instead of decoding the sensor data.
    pub fn is_indexed(self) -> bool {
        matches!(self, Self::Tiff | Self::Heif)
    }
}

pub fn sniff(head: &[u8]) -> Container {
    if head.len() < 12 {
        return Container::Unknown;
    }
    match () {
        _ if head.starts_with(&[0xFF, 0xD8, 0xFF]) => Container::Jpeg,
        _ if head.starts_with(b"\x89PNG\r\n\x1a\n") => Container::Png,
        _ if head.starts_with(b"II\x2a\x00") || head.starts_with(b"MM\x00\x2a") => Container::Tiff,
        // BigTIFF, used by some large scans.
        _ if head.starts_with(b"II\x2b\x00") || head.starts_with(b"MM\x00\x2b") => Container::Tiff,
        _ if head.starts_with(b"BM") => Container::Bmp,
        _ if &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" => Container::WebP,
        _ if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") => Container::Gif,
        _ if head.starts_with(b"8BPS") => Container::Psd,
        _ if &head[4..8] == b"ftyp" && is_heif_brand(&head[8..12]) => Container::Heif,
        _ => Container::Unknown,
    }
}

fn is_heif_brand(brand: &[u8]) -> bool {
    matches!(
        brand,
        b"heic" | b"heix" | b"hevc" | b"heim" | b"heis" | b"mif1" | b"msf1" | b"avif" | b"avis"
    )
}

/// Extension hint, used only to order work and to spot mismatches.
pub fn hint_from_extension(name: &str) -> Option<Container> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" | "jpe" => Container::Jpeg,
        "png" => Container::Png,
        "tif" | "tiff" | "arw" | "cr2" | "cr3" | "nef" | "dng" | "orf" | "rw2" | "raf" | "pef"
        | "srw" | "arq" => Container::Tiff,
        "bmp" => Container::Bmp,
        "webp" => Container::WebP,
        "gif" => Container::Gif,
        "psd" => Container::Psd,
        "heic" | "heif" | "avif" => Container::Heif,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(prefix: &[u8]) -> Vec<u8> {
        let mut v = prefix.to_vec();
        v.resize(32, 0);
        v
    }

    #[test]
    fn detects_the_containers_in_this_archive() {
        assert_eq!(sniff(&pad(&[0xFF, 0xD8, 0xFF, 0xE1])), Container::Jpeg);
        assert_eq!(sniff(&pad(b"II\x2a\x00")), Container::Tiff);
        assert_eq!(sniff(&pad(b"MM\x00\x2a")), Container::Tiff);
        assert_eq!(sniff(&pad(b"\x89PNG\r\n\x1a\n")), Container::Png);
        assert_eq!(sniff(&pad(b"BM")), Container::Bmp);
        assert_eq!(sniff(&pad(b"8BPS")), Container::Psd);
    }

    #[test]
    fn detects_riff_and_isobmff_which_need_a_second_field() {
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff(&pad(&webp)), Container::WebP);

        let mut heic = vec![0, 0, 0, 0x18];
        heic.extend_from_slice(b"ftypheic");
        assert_eq!(sniff(&pad(&heic)), Container::Heif);
    }

    #[test]
    fn a_wrong_extension_does_not_decide_the_container() {
        // An extensionless JPEG, which this archive has 24k of.
        assert_eq!(sniff(&pad(&[0xFF, 0xD8, 0xFF])), Container::Jpeg);
        assert_eq!(hint_from_extension("preview1"), None);
        // ARW is a TIFF container.
        assert_eq!(hint_from_extension("DSC01234.ARW"), Some(Container::Tiff));
    }

    #[test]
    fn short_and_unknown_input_is_not_an_image() {
        assert_eq!(sniff(b"abc"), Container::Unknown);
        assert_eq!(sniff(&pad(b"not an image at all")), Container::Unknown);
    }
}
