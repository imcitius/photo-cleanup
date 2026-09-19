//! Metadata, including the provenance links that make derivative detection
//! exact rather than a guess.

use crate::sniff::Container;
use std::io::Cursor;

/// Where a capture date came from, in descending order of trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum DateSource {
    Exif,
    Digitized,
    FileDateTime,
    Filename,
    #[default]
    None,
}

impl DateSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exif => "exif",
            Self::Digitized => "digitized",
            Self::FileDateTime => "file-datetime",
            Self::Filename => "filename",
            Self::None => "none",
        }
    }
}

/// Exact links between a derivative and what it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    /// `xmpMM:DocumentID` — identity of this rendition's document.
    pub document_id: Option<String>,
    /// `xmpMM:OriginalDocumentID` — identity of the original it descends from.
    pub original_document_id: Option<String>,
    /// `xmpMM:DerivedFrom` — the immediate parent.
    pub derived_from: Option<String>,
    /// DNG tag 0xC68B: the raw file this DNG was converted from, by name.
    pub dng_original_raw: Option<String>,
}

impl Provenance {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImageMeta {
    pub orientation: u16,
    pub taken_at: Option<i64>,
    pub date_source: DateSource,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub body_serial: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub f_number: Option<f64>,
    pub focal_length: Option<f64>,
    pub exposure: Option<String>,
    pub gps: Option<(f64, f64)>,
    /// `Software` / `CreatorTool`: names the editor that wrote the file.
    pub software: Option<String>,
    /// Dimensions of the photograph itself, when the container records them
    /// separately from whatever preview we decoded.
    pub raw_dimensions: Option<(u32, u32)>,
    pub provenance: Provenance,
}

fn ascii(f: &exif::Field) -> Option<String> {
    match &f.value {
        exif::Value::Ascii(v) => v
            .first()
            .map(|b| String::from_utf8_lossy(b).trim().to_string())
            .filter(|s| !s.is_empty()),
        _ => None,
    }
}

fn get_str(x: &exif::Exif, tag: exif::Tag) -> Option<String> {
    x.get_field(tag, exif::In::PRIMARY).and_then(ascii)
}

fn get_uint(x: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    x.get_field(tag, exif::In::PRIMARY)?.value.get_uint(0)
}

fn get_f64(x: &exif::Exif, tag: exif::Tag) -> Option<f64> {
    match &x.get_field(tag, exif::In::PRIMARY)?.value {
        exif::Value::Rational(v) => v.first().map(|r| r.to_f64()),
        exif::Value::SRational(v) => v.first().map(|r| r.to_f64()),
        other => other.get_uint(0).map(|u| u as f64),
    }
}

/// Days since the Unix epoch. Howard Hinnant's civil-date algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// EXIF writes `YYYY:MM:DD HH:MM:SS`, interpreted as UTC: the true offset is
/// unknowable from the tag alone, and consistency is what grouping needs.
pub fn parse_exif_datetime(s: &str) -> Option<i64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.trim().parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // Cameras with a dead clock write 1970 or earlier; treat as unknown.
    if !(1980..=2100).contains(&y) {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

/// A date embedded in a filename, for the many files that lost their EXIF:
/// `IMG_20190714_123456.jpg`, `Screenshot_2021-03-02`, `2019.07.14 отпуск`.
pub fn date_from_name(name: &str) -> Option<i64> {
    let b = name.as_bytes();
    for i in 0..b.len().saturating_sub(7) {
        if !b[i].is_ascii_digit() {
            continue;
        }
        // Do not start mid-number.
        if i > 0 && b[i - 1].is_ascii_digit() {
            continue;
        }
        let digits: Vec<u8> = b[i..]
            .iter()
            .take_while(|c| c.is_ascii_digit() || matches!(c, b'-' | b'.' | b'_' | b'/'))
            .copied()
            .filter(|c| c.is_ascii_digit())
            .take(8)
            .collect();
        if digits.len() < 8 {
            continue;
        }
        let s: String = digits.iter().map(|c| *c as char).collect();
        let y: i64 = s[0..4].parse().ok()?;
        let mo: i64 = s[4..6].parse().ok()?;
        let d: i64 = s[6..8].parse().ok()?;
        if (1990..=2100).contains(&y) && (1..=12).contains(&mo) && (1..=31).contains(&d) {
            return Some(days_from_civil(y, mo, d) * 86_400);
        }
    }
    None
}

fn gps(x: &exif::Exif) -> Option<(f64, f64)> {
    let dms = |tag: exif::Tag| -> Option<f64> {
        match &x.get_field(tag, exif::In::PRIMARY)?.value {
            exif::Value::Rational(v) if v.len() >= 3 => {
                Some(v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0)
            }
            _ => None,
        }
    };
    let lat = dms(exif::Tag::GPSLatitude)?;
    let lon = dms(exif::Tag::GPSLongitude)?;
    let neg = |v: f64, r: Option<String>, m: &str| {
        if r.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(m)) {
            -v
        } else {
            v
        }
    };
    let lat = neg(lat, get_str(x, exif::Tag::GPSLatitudeRef), "S");
    let lon = neg(lon, get_str(x, exif::Tag::GPSLongitudeRef), "W");
    (lat != 0.0 || lon != 0.0).then_some((lat, lon))
}

/// Pull out the XMP packet, wherever in the file it sits.
pub fn find_xmp(bytes: &[u8]) -> Option<&[u8]> {
    let start = find(bytes, b"<x:xmpmeta")?;
    let end = find(&bytes[start..], b"</x:xmpmeta>")? + start + b"</x:xmpmeta>".len();
    Some(&bytes[start..end])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Adobe's provenance chain, which appears both as attributes and as nested
/// elements depending on which tool wrote it.
pub fn parse_xmp(xmp: &[u8]) -> (Provenance, Option<String>) {
    use quick_xml::events::Event;
    let mut p = Provenance::default();
    let mut creator_tool = None;
    let mut reader = quick_xml::Reader::from_reader(xmp);
    reader.config_mut().trim_text(true);

    let mut in_element: Option<String> = None;
    let mut buf = Vec::new();

    let take = |name: &str, val: String, p: &mut Provenance, tool: &mut Option<String>| {
        let val = val.trim().to_string();
        if val.is_empty() {
            return;
        }
        match name {
            "xmpMM:DocumentID" => p.document_id.get_or_insert(val),
            "xmpMM:OriginalDocumentID" => p.original_document_id.get_or_insert(val),
            "xmpMM:DerivedFrom" | "stRef:documentID" => p.derived_from.get_or_insert(val),
            "xmp:CreatorTool" | "tiff:Software" => tool.get_or_insert(val),
            _ => return,
        };
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                    let val = String::from_utf8_lossy(&attr.value).to_string();
                    take(&key, val, &mut p, &mut creator_tool);
                }
                in_element = Some(name);
            }
            Ok(Event::Text(t)) => {
                if let Some(name) = &in_element {
                    let val = String::from_utf8_lossy(t.as_ref()).to_string();
                    take(name, val, &mut p, &mut creator_tool);
                }
            }
            Ok(Event::End(_)) => in_element = None,
            _ => {}
        }
        buf.clear();
    }
    (p, creator_tool)
}

pub fn read(bytes: &[u8], container: Container) -> ImageMeta {
    let mut m = ImageMeta {
        orientation: 1,
        ..Default::default()
    };

    if let Ok(x) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) {
        m.orientation = get_uint(&x, exif::Tag::Orientation).unwrap_or(1) as u16;
        m.camera_make = get_str(&x, exif::Tag::Make);
        m.camera_model = get_str(&x, exif::Tag::Model);
        m.body_serial = get_str(&x, exif::Tag::BodySerialNumber);
        m.lens = get_str(&x, exif::Tag::LensModel);
        m.iso = get_uint(&x, exif::Tag::PhotographicSensitivity);
        m.f_number = get_f64(&x, exif::Tag::FNumber);
        m.focal_length = get_f64(&x, exif::Tag::FocalLength);
        m.exposure = x
            .get_field(exif::Tag::ExposureTime, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string());
        m.software = get_str(&x, exif::Tag::Software);
        m.gps = gps(&x);

        for (tag, source) in [
            (exif::Tag::DateTimeOriginal, DateSource::Exif),
            (exif::Tag::DateTimeDigitized, DateSource::Digitized),
            (exif::Tag::DateTime, DateSource::FileDateTime),
        ] {
            if m.taken_at.is_some() {
                break;
            }
            if let Some(ts) = get_str(&x, tag).as_deref().and_then(parse_exif_datetime) {
                m.taken_at = Some(ts);
                m.date_source = source;
            }
        }

        // For a raw file the primary directory describes the sensor frame,
        // which is the photograph — not the preview we decoded.
        if container.is_indexed() {
            if let (Some(w), Some(h)) = (
                get_uint(&x, exif::Tag::PixelXDimension)
                    .or_else(|| get_uint(&x, exif::Tag::ImageWidth)),
                get_uint(&x, exif::Tag::PixelYDimension)
                    .or_else(|| get_uint(&x, exif::Tag::ImageLength)),
            ) {
                if w > 0 && h > 0 {
                    m.raw_dimensions = Some((w, h));
                }
            }
        }

        // DNG records the name of the raw file it was converted from, which
        // links a converted copy to its original with no guessing at all.
        let dng_tag = exif::Tag(exif::Context::Tiff, 0xC68B);
        m.provenance.dng_original_raw = get_str(&x, dng_tag);
    }

    if let Some(xmp) = find_xmp(bytes) {
        let (prov, tool) = parse_xmp(xmp);
        if m.provenance.document_id.is_none() {
            m.provenance.document_id = prov.document_id;
        }
        if m.provenance.original_document_id.is_none() {
            m.provenance.original_document_id = prov.original_document_id;
        }
        if m.provenance.derived_from.is_none() {
            m.provenance.derived_from = prov.derived_from;
        }
        if m.software.is_none() {
            m.software = tool;
        }
    }

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exif_timestamps_and_rejects_nonsense() {
        assert_eq!(
            parse_exif_datetime("2019:07:14 18:32:05"),
            Some(1_563_129_125)
        );
        assert_eq!(
            parse_exif_datetime("1970:01:01 00:00:00"),
            None,
            "мёртвые часы"
        );
        assert_eq!(parse_exif_datetime("    :  :     :  :  "), None);
        assert_eq!(parse_exif_datetime("2019:13:14 18:32:05"), None);
        assert_eq!(parse_exif_datetime("short"), None);
    }

    #[test]
    fn epoch_arithmetic_matches_known_dates() {
        // 1970 is rejected as a dead camera clock, so the arithmetic is
        // checked on dates the parser accepts.
        assert_eq!(
            parse_exif_datetime("2000:03:01 00:00:00"),
            Some(951_868_800)
        );
        assert_eq!(
            parse_exif_datetime("2024:02:29 12:00:00"),
            Some(1_709_208_000)
        );
        let a = parse_exif_datetime("1980:01:01 00:00:00").unwrap();
        let b = parse_exif_datetime("1980:01:02 00:00:00").unwrap();
        assert_eq!(b - a, 86_400);
    }

    #[test]
    fn reads_dates_out_of_the_filenames_this_archive_uses() {
        let day = |y, m, d| Some(days_from_civil(y, m, d) * 86_400);
        assert_eq!(date_from_name("IMG_20190714_123456.jpg"), day(2019, 7, 14));
        assert_eq!(date_from_name("Screenshot_2021-03-02.png"), day(2021, 3, 2));
        assert_eq!(date_from_name("2019.07.14 отпуск"), day(2019, 7, 14));
        assert_eq!(date_from_name("DSC01234.ARW"), None);
        assert_eq!(date_from_name("99999999.jpg"), None);
    }

    #[test]
    fn extracts_the_adobe_provenance_chain() {
        let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
          <rdf:RDF><rdf:Description
             xmp:CreatorTool="Adobe Photoshop Lightroom Classic 13.2"
             xmpMM:DocumentID="xmp.did:AAAA"
             xmpMM:OriginalDocumentID="xmp.did:ORIG"
             xmpMM:DerivedFrom="xmp.did:PARENT"/></rdf:RDF></x:xmpmeta>"#;
        let (p, tool) = parse_xmp(xmp);
        assert_eq!(p.document_id.as_deref(), Some("xmp.did:AAAA"));
        assert_eq!(p.original_document_id.as_deref(), Some("xmp.did:ORIG"));
        assert_eq!(p.derived_from.as_deref(), Some("xmp.did:PARENT"));
        assert!(tool.unwrap().contains("Lightroom"));
    }

    #[test]
    fn handles_derived_from_written_as_a_nested_element() {
        let xmp = br#"<x:xmpmeta><rdf:RDF><rdf:Description>
            <xmpMM:DerivedFrom stRef:documentID="xmp.did:NESTED"/>
          </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (p, _) = parse_xmp(xmp);
        assert_eq!(p.derived_from.as_deref(), Some("xmp.did:NESTED"));
    }

    #[test]
    fn finds_an_xmp_packet_inside_surrounding_bytes() {
        let mut buf = vec![0xFFu8; 100];
        buf.extend_from_slice(br#"<x:xmpmeta a="b"></x:xmpmeta>"#);
        buf.extend_from_slice(&[0u8; 50]);
        let found = find_xmp(&buf).unwrap();
        assert!(found.starts_with(b"<x:xmpmeta"));
        assert!(found.ends_with(b"</x:xmpmeta>"));
        assert!(find_xmp(b"no packet here").is_none());
    }
}
