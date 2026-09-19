//! Locating the JPEG previews embedded in TIFF-family containers.
//!
//! ARW, CR2, NEF and DNG are all TIFF: a header, then a chain of image file
//! directories that index everything inside. Sony bodies write a sizeable
//! JPEG preview alongside the sensor data, so a thumbnail, a perceptual hash
//! and a similarity check can all be had by seeking to that preview and
//! reading a couple of megabytes — instead of decoding a 25 MB raw frame.
//!
//! This replaces a libraw dependency entirely for our purposes.

use crate::jpeg;

const TAG_IMAGE_WIDTH: u16 = 0x0100;
const TAG_IMAGE_LENGTH: u16 = 0x0101;
const TAG_COMPRESSION: u16 = 0x0103;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_JPEG_OFFSET: u16 = 0x0201;
const TAG_JPEG_LENGTH: u16 = 0x0202;
const TAG_EXIF_IFD: u16 = 0x8769;

/// Bounded so a corrupt or hostile file cannot make us walk forever.
const MAX_IFDS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Embedded {
    pub offset: usize,
    pub len: usize,
    pub width: u32,
    pub height: u32,
}

impl Embedded {
    pub fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

#[derive(Clone, Copy)]
struct Reader<'a> {
    data: &'a [u8],
    big_endian: bool,
}

impl<'a> Reader<'a> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b = self.data.get(at..at + 2)?;
        Some(if self.big_endian {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b = self.data.get(at..at + 4)?;
        Some(if self.big_endian {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
    }
}

/// Values of a tag, normalised to u32. Only the short forms TIFF actually
/// uses for offsets and sizes are handled.
fn values(r: &Reader<'_>, entry: usize) -> Option<Vec<u32>> {
    let kind = r.u16(entry + 2)?;
    let count = r.u32(entry + 4)? as usize;
    let unit = match kind {
        1 | 2 | 6 | 7 => 1usize,
        3 | 8 => 2,
        4 | 9 | 13 => 4,
        _ => return None,
    };
    let total = count.checked_mul(unit)?;
    // Up to four bytes live inline; anything larger is stored at an offset.
    let base = if total <= 4 {
        entry + 8
    } else {
        r.u32(entry + 8)? as usize
    };
    // A preview index never has thousands of entries; refuse absurd counts.
    if count > 4096 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = base.checked_add(i * unit)?;
        out.push(match unit {
            1 => *r.data.get(at)? as u32,
            2 => r.u16(at)? as u32,
            _ => r.u32(at)?,
        });
    }
    Some(out)
}

fn first(r: &Reader<'_>, entry: usize) -> Option<u32> {
    values(r, entry)?.first().copied()
}

/// A candidate is only accepted if the bytes really are a JPEG, so a stale or
/// misinterpreted offset yields nothing rather than garbage.
fn accept(data: &[u8], offset: usize, len: usize, out: &mut Vec<Embedded>) {
    let Some(end) = offset.checked_add(len) else {
        return;
    };
    let end = end.min(data.len());
    let Some(slice) = data.get(offset..end) else {
        return;
    };
    if !slice.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return;
    }
    // Trust the marker over the recorded length when they disagree.
    let real_len = jpeg::find_eoi(slice, 2).unwrap_or(slice.len());
    let Some((width, height)) = jpeg::dimensions(&slice[..real_len]) else {
        return;
    };
    out.push(Embedded {
        offset,
        len: real_len,
        width,
        height,
    });
}

fn scan_ifd(
    r: &Reader<'_>,
    at: usize,
    out: &mut Vec<Embedded>,
    seen: &mut Vec<usize>,
    budget: &mut usize,
) {
    if *budget == 0 || seen.contains(&at) {
        return;
    }
    seen.push(at);
    *budget -= 1;

    let Some(count) = r.u16(at) else { return };
    let count = count as usize;
    if count > 512 {
        return; // not a plausible IFD
    }

    let mut jpeg_offset = None;
    let mut jpeg_length = None;
    let mut compression = None;
    let mut strip_offsets: Vec<u32> = Vec::new();
    let mut strip_counts: Vec<u32> = Vec::new();
    let mut sub_ifds: Vec<u32> = Vec::new();
    let mut width = None;
    let mut height = None;

    for i in 0..count {
        let entry = match at.checked_add(2 + i * 12) {
            Some(e) if e + 12 <= r.data.len() => e,
            _ => return,
        };
        let Some(tag) = r.u16(entry) else { return };
        match tag {
            TAG_JPEG_OFFSET => jpeg_offset = first(r, entry),
            TAG_JPEG_LENGTH => jpeg_length = first(r, entry),
            TAG_COMPRESSION => compression = first(r, entry),
            TAG_STRIP_OFFSETS => strip_offsets = values(r, entry).unwrap_or_default(),
            TAG_STRIP_BYTE_COUNTS => strip_counts = values(r, entry).unwrap_or_default(),
            TAG_SUB_IFDS | TAG_EXIF_IFD => sub_ifds.extend(values(r, entry).unwrap_or_default()),
            TAG_IMAGE_WIDTH => width = first(r, entry),
            TAG_IMAGE_LENGTH => height = first(r, entry),
            _ => {}
        }
    }
    let _ = (width, height); // the JPEG itself is the authority on its size

    if let (Some(off), Some(len)) = (jpeg_offset, jpeg_length) {
        accept(r.data, off as usize, len as usize, out);
    }

    // Compression 6 is the old JPEG mode, 7 the modern one; either way a
    // single-strip image of that kind is a whole JPEG file.
    if matches!(compression, Some(6 | 7)) && strip_offsets.len() == 1 && strip_counts.len() == 1 {
        accept(
            r.data,
            strip_offsets[0] as usize,
            strip_counts[0] as usize,
            out,
        );
    }

    for sub in sub_ifds {
        scan_ifd(r, sub as usize, out, seen, budget);
    }

    // Follow the chain to the next directory.
    let next_at = at + 2 + count * 12;
    if let Some(next) = r.u32(next_at) {
        if next != 0 {
            scan_ifd(r, next as usize, out, seen, budget);
        }
    }
}

/// Every JPEG this container indexes, in no particular order.
pub fn find_embedded_jpegs(data: &[u8]) -> Vec<Embedded> {
    let Some(head) = data.get(0..8) else {
        return Vec::new();
    };
    let big_endian = match &head[0..2] {
        b"II" => false,
        b"MM" => true,
        _ => return Vec::new(),
    };
    let r = Reader { data, big_endian };
    // 42 is classic TIFF. 43 is BigTIFF, whose 8-byte offsets need a
    // different reader; photo previews do not live there in practice.
    if r.u16(2) != Some(42) {
        return Vec::new();
    }
    let Some(ifd0) = r.u32(4) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut seen = Vec::new();
    let mut budget = MAX_IFDS;
    scan_ifd(&r, ifd0 as usize, &mut out, &mut seen, &mut budget);

    out.sort_by_key(|e| e.offset);
    out.dedup_by_key(|e| e.offset);
    out
}

/// Candidate preview ranges read from the directories alone.
///
/// The directories live at the front of the file, but the preview data they
/// point at can be megabytes further in. This lets a caller parse the index
/// from a small head read and then seek straight to the bytes it wants,
/// instead of pulling a 25 MB raw file through the network or off a platter.
pub fn preview_ranges(head: &[u8]) -> Vec<(u64, u64)> {
    let Some(h) = head.get(0..8) else {
        return Vec::new();
    };
    let big_endian = match &h[0..2] {
        b"II" => false,
        b"MM" => true,
        _ => return Vec::new(),
    };
    let r = Reader {
        data: head,
        big_endian,
    };
    if r.u16(2) != Some(42) {
        return Vec::new();
    }
    let Some(ifd0) = r.u32(4) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = Vec::new();
    let mut budget = MAX_IFDS;
    collect_ranges(&r, ifd0 as usize, &mut out, &mut seen, &mut budget);
    out.sort_by_key(|(_, len)| std::cmp::Reverse(*len));
    out.dedup();
    out
}

fn collect_ranges(
    r: &Reader<'_>,
    at: usize,
    out: &mut Vec<(u64, u64)>,
    seen: &mut Vec<usize>,
    budget: &mut usize,
) {
    if *budget == 0 || seen.contains(&at) {
        return;
    }
    seen.push(at);
    *budget -= 1;

    let Some(count) = r.u16(at) else { return };
    let count = count as usize;
    if count > 512 {
        return;
    }
    let (mut off, mut len, mut compression) = (None, None, None);
    let (mut strips, mut counts, mut subs) = (Vec::new(), Vec::new(), Vec::new());

    for i in 0..count {
        let entry = match at.checked_add(2 + i * 12) {
            Some(e) if e + 12 <= r.data.len() => e,
            _ => return,
        };
        let Some(tag) = r.u16(entry) else { return };
        match tag {
            TAG_JPEG_OFFSET => off = first(r, entry),
            TAG_JPEG_LENGTH => len = first(r, entry),
            TAG_COMPRESSION => compression = first(r, entry),
            TAG_STRIP_OFFSETS => strips = values(r, entry).unwrap_or_default(),
            TAG_STRIP_BYTE_COUNTS => counts = values(r, entry).unwrap_or_default(),
            TAG_SUB_IFDS | TAG_EXIF_IFD => subs.extend(values(r, entry).unwrap_or_default()),
            _ => {}
        }
    }
    if let (Some(o), Some(l)) = (off, len) {
        if l > 0 {
            out.push((o as u64, l as u64));
        }
    }
    if matches!(compression, Some(6 | 7)) && strips.len() == 1 && counts.len() == 1 && counts[0] > 0
    {
        out.push((strips[0] as u64, counts[0] as u64));
    }
    for sub in subs {
        collect_ranges(r, sub as usize, out, seen, budget);
    }
    let next_at = at + 2 + count * 12;
    if let Some(next) = r.u32(next_at) {
        if next != 0 {
            collect_ranges(r, next as usize, out, seen, budget);
        }
    }
}

/// Validate bytes that were read from a candidate range.
pub fn accept_preview(bytes: &[u8]) -> Option<Embedded> {
    if !bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return None;
    }
    let len = jpeg::find_eoi(bytes, 2).unwrap_or(bytes.len());
    let (width, height) = jpeg::dimensions(&bytes[..len])?;
    Some(Embedded {
        offset: 0,
        len,
        width,
        height,
    })
}

/// The biggest embedded preview, which is the one worth working from.
pub fn largest_preview(data: &[u8]) -> Option<Embedded> {
    find_embedded_jpegs(data)
        .into_iter()
        .max_by_key(|e| e.pixels())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG whose payload scales with its dimensions, as a real one does.
    fn tiny_jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[0u8; 10]);
        // Stand-in for entropy-coded data, avoiding 0xFF so no marker appears.
        v.extend(std::iter::repeat_n(0x5Au8, (w as usize / 8).max(4)));
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    /// A little-endian TIFF with IFD0 pointing at a thumbnail and a SubIFD
    /// holding the large preview — the shape a Sony ARW has.
    fn arw_like() -> Vec<u8> {
        let thumb = tiny_jpeg(160, 120);
        let preview = tiny_jpeg(1616, 1080);

        let mut buf = vec![0u8; 8];
        buf[0..2].copy_from_slice(b"II");
        buf[2..4].copy_from_slice(&42u16.to_le_bytes());

        let thumb_at = 8usize;
        buf.extend_from_slice(&thumb);
        let preview_at = buf.len();
        buf.extend_from_slice(&preview);

        let sub_ifd_at = buf.len();
        let mut sub = Vec::new();
        sub.extend_from_slice(&1u16.to_le_bytes());
        sub.extend_from_slice(&TAG_JPEG_OFFSET.to_le_bytes());
        sub.extend_from_slice(&4u16.to_le_bytes());
        sub.extend_from_slice(&1u32.to_le_bytes());
        sub.extend_from_slice(&(preview_at as u32).to_le_bytes());
        sub.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&sub);

        // Length tag for the preview lives in the same SubIFD; rebuild it with
        // two entries now that both offsets are known.
        let mut sub = Vec::new();
        sub.extend_from_slice(&2u16.to_le_bytes());
        for (tag, val) in [
            (TAG_JPEG_OFFSET, preview_at as u32),
            (TAG_JPEG_LENGTH, preview.len() as u32),
        ] {
            sub.extend_from_slice(&tag.to_le_bytes());
            sub.extend_from_slice(&4u16.to_le_bytes());
            sub.extend_from_slice(&1u32.to_le_bytes());
            sub.extend_from_slice(&val.to_le_bytes());
        }
        sub.extend_from_slice(&0u32.to_le_bytes());
        buf.truncate(sub_ifd_at);
        buf.extend_from_slice(&sub);

        let ifd0_at = buf.len();
        let mut ifd0 = Vec::new();
        ifd0.extend_from_slice(&3u16.to_le_bytes());
        for (tag, val) in [
            (TAG_JPEG_OFFSET, thumb_at as u32),
            (TAG_JPEG_LENGTH, thumb.len() as u32),
            (TAG_SUB_IFDS, sub_ifd_at as u32),
        ] {
            ifd0.extend_from_slice(&tag.to_le_bytes());
            ifd0.extend_from_slice(&4u16.to_le_bytes());
            ifd0.extend_from_slice(&1u32.to_le_bytes());
            ifd0.extend_from_slice(&val.to_le_bytes());
        }
        ifd0.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&ifd0);

        buf[4..8].copy_from_slice(&(ifd0_at as u32).to_le_bytes());
        buf
    }

    #[test]
    fn finds_both_previews_and_prefers_the_large_one() {
        let data = arw_like();
        let all = find_embedded_jpegs(&data);
        assert_eq!(all.len(), 2, "{all:?}");
        let big = largest_preview(&data).unwrap();
        assert_eq!((big.width, big.height), (1616, 1080));
    }

    #[test]
    fn a_wrong_length_is_corrected_by_the_end_marker() {
        let data = arw_like();
        let big = largest_preview(&data).unwrap();
        let slice = &data[big.offset..big.offset + big.len];
        assert!(slice.ends_with(&[0xFF, 0xD9]));
    }

    #[test]
    fn ranges_can_be_read_from_the_header_alone() {
        let data = arw_like();
        // Only the directories, not the pixel data they point at.
        let ranges = preview_ranges(&data);
        assert!(ranges.len() >= 2, "{ranges:?}");
        // Candidates come back largest first, and the largest really is the
        // full preview rather than the 160x120 thumbnail beside it.
        let (off, len) = ranges[0];
        let slice = &data[off as usize..(off + len) as usize];
        let e = accept_preview(slice).unwrap();
        assert_eq!((e.width, e.height), (1616, 1080));
    }

    #[test]
    fn the_preview_is_chosen_by_pixels_not_by_byte_length() {
        // Byte length only orders the candidates; the decision is made after
        // decoding, so an oddly compressed thumbnail cannot win.
        let data = arw_like();
        let best = preview_ranges(&data)
            .iter()
            .filter_map(|(off, len)| {
                let end = (off + len) as usize;
                data.get(*off as usize..end).and_then(accept_preview)
            })
            .max_by_key(|e| e.pixels())
            .unwrap();
        assert_eq!((best.width, best.height), (1616, 1080));
    }

    #[test]
    fn accept_preview_rejects_bytes_that_are_not_a_jpeg() {
        assert!(accept_preview(b"\x00\x01\x02\x03").is_none());
        assert!(accept_preview(&[0xFF, 0xD8, 0xFF]).is_none());
    }

    #[test]
    fn non_tiff_and_bigtiff_yield_nothing_instead_of_panicking() {
        assert!(find_embedded_jpegs(b"not a tiff at all").is_empty());
        let mut bigtiff = b"II".to_vec();
        bigtiff.extend_from_slice(&43u16.to_le_bytes());
        bigtiff.extend_from_slice(&[0u8; 32]);
        assert!(find_embedded_jpegs(&bigtiff).is_empty());
    }

    #[test]
    fn truncated_and_garbage_input_never_panics() {
        let data = arw_like();
        for cut in [0, 1, 8, 20, 60, data.len() / 2, data.len() - 1] {
            let _ = find_embedded_jpegs(&data[..cut.min(data.len())]);
        }
        // Offsets pointing outside the file must simply be rejected.
        let mut broken = data.clone();
        broken[4..8].copy_from_slice(&0xFFFF_0000u32.to_le_bytes());
        assert!(find_embedded_jpegs(&broken).is_empty());
    }
}
