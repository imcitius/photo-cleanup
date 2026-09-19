//! Reading just enough of each file.
//!
//! A raw file is 25 MB of sensor data wrapped around a JPEG preview of one or
//! two. The directories that say where that preview sits are at the front, so
//! one small head read plus one seek replaces reading the whole file — the
//! single largest saving in the pipeline.

use anyhow::{bail, Result};
use pc_image::{sniff, tiff, Container, HEAD_BYTES};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes hashed from each end for the partial content hash.
const EDGE: usize = 64 * 1024;

/// Above this a full decode is refused: a single frame would allocate more
/// than its share of a 16 GiB machine shared with other work.
pub const MAX_FULL_READ: u64 = 256 * 1024 * 1024;

/// Reading beyond this from one candidate range is not worth it; real
/// previews are one to three megabytes.
const MAX_PREVIEW_READ: u64 = 48 * 1024 * 1024;

/// How many candidate ranges to try before giving up on the index.
const MAX_CANDIDATES: usize = 4;

pub struct Read1 {
    /// The front of the file, or all of it when it is small or not indexed.
    pub head: Vec<u8>,
    /// The embedded preview, when the container indexed one.
    pub preview: Option<Vec<u8>>,
    pub container: Container,
    /// Size plus both ends: near-certain identity for photographs, without
    /// reading gigabytes. The full hash is computed only before acting.
    pub partial_hash: [u8; 32],
    /// True when `head` holds the entire file, so a full hash is free.
    pub complete: bool,
    pub bytes_read: u64,
}

fn read_at(f: &mut File, off: u64, len: usize) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(off))?;
    let mut buf = vec![0u8; len];
    let mut got = 0;
    while got < len {
        match f.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    buf.truncate(got);
    Ok(buf)
}

fn partial(size: u64, head: &[u8], tail: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&size.to_le_bytes());
    h.update(&head[..head.len().min(EDGE)]);
    h.update(tail);
    *h.finalize().as_bytes()
}

pub fn read_for_probe(path: &Path, size: u64) -> Result<Read1> {
    let mut f = File::open(path)?;
    let head_len = (size as usize).min(HEAD_BYTES);
    let head = read_at(&mut f, 0, head_len)?;
    let mut bytes_read = head.len() as u64;

    let container = sniff::sniff(&head);
    if !container.is_image() {
        bail!("не изображение");
    }

    let complete_head = size <= HEAD_BYTES as u64;

    // Indexed containers: take the preview and leave the sensor data alone.
    let mut preview = None;
    if container.is_indexed() {
        let mut best: Option<(Vec<u8>, u64)> = None;
        for (off, len) in tiff::preview_ranges(&head).into_iter().take(MAX_CANDIDATES) {
            if len > MAX_PREVIEW_READ || off >= size {
                continue;
            }
            let want = len.min(size - off) as usize;
            let bytes = read_at(&mut f, off, want)?;
            bytes_read += bytes.len() as u64;
            // Byte length only orders the candidates; pixels decide.
            if let Some(e) = tiff::accept_preview(&bytes) {
                if e.pixels() >= pc_image::thumb::MIN_PREVIEW_PIXELS
                    && best.as_ref().is_none_or(|(_, p)| e.pixels() > *p)
                {
                    best = Some((bytes[..e.len].to_vec(), e.pixels()));
                }
            }
        }
        preview = best.map(|(b, _)| b);
    }

    // Anything without a usable preview has to be decoded from the file.
    let (head, complete) = if preview.is_some() || complete_head {
        (head, complete_head)
    } else {
        if size > MAX_FULL_READ {
            bail!("файл слишком велик для полного чтения ({size} байт)");
        }
        let all = read_at(&mut f, 0, size as usize)?;
        bytes_read += all.len().saturating_sub(head_len) as u64;
        (all, true)
    };

    let tail = if complete {
        let from = head.len().saturating_sub(EDGE);
        head[from..].to_vec()
    } else {
        let off = size.saturating_sub(EDGE as u64);
        let t = read_at(&mut f, off, EDGE)?;
        bytes_read += t.len() as u64;
        t
    };

    Ok(Read1 {
        partial_hash: partial(size, &head, &tail),
        complete,
        bytes_read,
        head,
        preview,
        container,
    })
}

/// Full content hash, computed only when a file is about to be acted on.
pub fn full_hash(path: &Path) -> Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        match f.read(&mut buf)? {
            0 => break,
            n => h.update(&buf[..n]),
        };
    }
    Ok(*h.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn jpeg_file(dir: &Path, name: &str, pad: usize) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = File::create(&p).unwrap();
        f.write_all(&[0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08])
            .unwrap();
        f.write_all(&64u16.to_be_bytes()).unwrap();
        f.write_all(&64u16.to_be_bytes()).unwrap();
        f.write_all(&[0u8; 10]).unwrap();
        f.write_all(&vec![0x5A; pad]).unwrap();
        f.write_all(&[0xFF, 0xD9]).unwrap();
        p
    }

    #[test]
    fn a_small_jpeg_is_read_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let p = jpeg_file(tmp.path(), "a.jpg", 1000);
        let size = std::fs::metadata(&p).unwrap().len();
        let r = read_for_probe(&p, size).unwrap();
        assert!(r.complete);
        assert_eq!(r.head.len() as u64, size);
        assert!(r.preview.is_none());
        assert_eq!(r.container, Container::Jpeg);
    }

    #[test]
    fn the_partial_hash_separates_files_that_differ_at_either_end() {
        let tmp = tempfile::tempdir().unwrap();
        let a = jpeg_file(tmp.path(), "a.jpg", 1000);
        let b = jpeg_file(tmp.path(), "b.jpg", 1001);
        let size = |p: &Path| std::fs::metadata(p).unwrap().len();
        let ra = read_for_probe(&a, size(&a)).unwrap();
        let rb = read_for_probe(&b, size(&b)).unwrap();
        assert_ne!(ra.partial_hash, rb.partial_hash);

        // The same bytes under a different name hash the same.
        let c = tmp.path().join("copy.jpg");
        std::fs::copy(&a, &c).unwrap();
        assert_eq!(
            read_for_probe(&c, size(&c)).unwrap().partial_hash,
            ra.partial_hash
        );
    }

    #[test]
    fn a_non_image_is_rejected_after_reading_only_its_head() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("notes.txt");
        std::fs::write(&p, vec![b'x'; 500_000]).unwrap();
        let size = std::fs::metadata(&p).unwrap().len();
        assert!(read_for_probe(&p, size).is_err());
    }

    #[test]
    fn full_hash_matches_a_direct_hash_of_the_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let p = jpeg_file(tmp.path(), "a.jpg", 5000);
        let want = blake3::hash(&std::fs::read(&p).unwrap());
        assert_eq!(full_hash(&p).unwrap(), *want.as_bytes());
    }
}
