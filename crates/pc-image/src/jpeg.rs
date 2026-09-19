//! Just enough JPEG parsing to size an image without decoding it.

/// Width and height from the first Start-Of-Frame marker.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2usize;
    while i + 3 < data.len() {
        if data[i] != 0xFF {
            i += 1; // resynchronise over fill bytes or padding
            continue;
        }
        let marker = data[i + 1];
        i += 2;
        match marker {
            // Standalone markers carry no length.
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 => return None, // end of image before any frame
            _ => {}
        }
        if i + 1 >= data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if len < 2 || i + len > data.len() {
            return None;
        }
        // SOF0..SOF15, minus the markers that share the range but are not frames.
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if len < 7 {
                return None;
            }
            let h = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
            let w = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            return (w > 0 && h > 0).then_some((w, h));
        }
        i += len;
    }
    None
}

/// Offset just past the End-Of-Image marker, used to bound an embedded
/// preview whose recorded length is wrong or missing.
pub fn find_eoi(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < data.len() {
        if data[i] == 0xFF && data[i + 1] == 0xD9 {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal JPEG: SOI, an APP0 to skip over, an SOF0 of 640x480, EOI.
    fn sample() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&480u16.to_be_bytes());
        v.extend_from_slice(&640u16.to_be_bytes());
        v.extend_from_slice(&[0u8; 10]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn reads_dimensions_past_other_segments() {
        assert_eq!(dimensions(&sample()), Some((640, 480)));
    }

    #[test]
    fn rejects_non_jpeg_and_truncated_input() {
        assert_eq!(dimensions(b"not a jpeg"), None);
        assert_eq!(dimensions(&[0xFF, 0xD8]), None);
        let s = sample();
        assert_eq!(dimensions(&s[..8]), None);
    }

    #[test]
    fn finds_the_end_marker() {
        let s = sample();
        assert_eq!(find_eoi(&s, 2), Some(s.len()));
        assert_eq!(find_eoi(&[0xFF, 0xD8, 0x00], 0), None);
    }
}
