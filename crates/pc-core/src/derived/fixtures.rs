//! Synthetic, structurally complete system junk for tests (el-126jk).
//!
//! A magic prefix is not a file: the rule in `pc_core::derived` validates the
//! whole structure a name claims, so a test that wants junk moved has to
//! hand it junk that is complete. These are built here, once, and included
//! by path from every crate whose tests need them — nothing of this is
//! compiled into a release binary.
#![allow(dead_code)]

fn be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn le16(v: &mut [u8], at: usize, x: u16) {
    v[at..at + 2].copy_from_slice(&x.to_le_bytes());
}

fn le32(v: &mut [u8], at: usize, x: u32) {
    v[at..at + 4].copy_from_slice(&x.to_le_bytes());
}

/// A Finder `.DS_Store` holding no records: the buddy allocator header, its
/// root block with the block table and the `DSDB` table of contents, the
/// `DSDB` header and an empty leaf node. 4100 bytes.
pub fn ds_store() -> Vec<u8> {
    // Offsets are relative to byte 4; a block address is offset | log2(size).
    const ROOT_OFF: u32 = 0x800;
    const ROOT_SIZE: u32 = 0x800;
    let addrs = [ROOT_OFF | 11, 0x20 | 5, 0x40 | 6];
    let mut f = vec![0u8; 4 + (ROOT_OFF + ROOT_SIZE) as usize];
    f[..4].copy_from_slice(&[0, 0, 0, 1]);
    let mut head = b"Bud1".to_vec();
    be32(&mut head, ROOT_OFF);
    be32(&mut head, ROOT_SIZE);
    be32(&mut head, ROOT_OFF);
    head.extend_from_slice(&[
        0, 0, 0x10, 0x0C, 0, 0, 0, 0x87, 0, 0, 0x20, 0x0B, 0, 0, 0, 0,
    ]);
    f[4..4 + head.len()].copy_from_slice(&head);

    let mut root = Vec::new();
    be32(&mut root, addrs.len() as u32);
    be32(&mut root, 0);
    for a in addrs {
        be32(&mut root, a);
    }
    root.resize(8 + 256 * 4, 0);
    be32(&mut root, 1);
    root.push(4);
    root.extend_from_slice(b"DSDB");
    be32(&mut root, 1);
    for _ in 0..32 {
        be32(&mut root, 0);
    }
    let at = 4 + ROOT_OFF as usize;
    f[at..at + root.len()].copy_from_slice(&root);

    let mut dsdb = Vec::new();
    for x in [2u32, 0, 0, 1, 0x1000] {
        be32(&mut dsdb, x);
    }
    f[4 + 0x20..4 + 0x20 + dsdb.len()].copy_from_slice(&dsdb);
    // Node 2 at 0x40: a leaf (P = 0) with no records — already zeros.
    f
}

/// An AppleDouble resource-fork file as macOS writes beside a file on a
/// foreign volume: header, one entry (resource fork, id 2), its 11 bytes.
/// The same bytes stand in for Synology's `@SynoEAStream`.
pub fn apple_double() -> Vec<u8> {
    let mut v = Vec::new();
    be32(&mut v, 0x0005_1607);
    be32(&mut v, 0x0002_0000);
    v.extend_from_slice(b"Mac OS X        ");
    v.extend_from_slice(&1u16.to_be_bytes());
    for x in [2u32, 38, 11] {
        be32(&mut v, x);
    }
    v.extend_from_slice(b"resource123");
    v
}

/// A Windows `Thumbs.db`: a version-3 compound file whose directory holds the
/// root entry and an empty `Catalog` stream. 1536 bytes.
pub fn thumbs_db() -> Vec<u8> {
    const END: u32 = 0xFFFF_FFFE;
    const FREE: u32 = 0xFFFF_FFFF;
    let mut f = vec![0u8; 512 * 3];
    f[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    le16(&mut f, 24, 0x3E);
    le16(&mut f, 26, 3);
    le16(&mut f, 28, 0xFFFE);
    le16(&mut f, 30, 9);
    le16(&mut f, 32, 6);
    le32(&mut f, 44, 1); // FAT sectors
    le32(&mut f, 48, 1); // first directory sector
    le32(&mut f, 56, 4096);
    le32(&mut f, 60, END);
    le32(&mut f, 68, END);
    le32(&mut f, 76, 0);
    for i in 1..109 {
        le32(&mut f, 76 + 4 * i, FREE);
    }
    // Sector 0: the FAT. Sector 0 is a FAT sector, sector 1 one directory sector.
    le32(&mut f, 512, 0xFFFF_FFFD);
    le32(&mut f, 516, END);
    for i in 2..128 {
        le32(&mut f, 512 + 4 * i, FREE);
    }
    // Sector 1: four directory entries.
    let entry = |f: &mut Vec<u8>, i: usize, name: &str, kind: u8, child: u32| {
        let at = 1024 + 128 * i;
        let units: Vec<u16> = name.encode_utf16().collect();
        for (k, u) in units.iter().enumerate() {
            le16(f, at + 2 * k, *u);
        }
        let len = if name.is_empty() {
            0
        } else {
            (units.len() as u16 + 1) * 2
        };
        le16(f, at + 64, len);
        f[at + 66] = kind;
        f[at + 67] = 1;
        le32(f, at + 68, FREE);
        le32(f, at + 72, FREE);
        le32(f, at + 76, child);
        le32(f, at + 116, if kind == 0 { 0 } else { END });
    };
    entry(&mut f, 0, "Root Entry", 5, 1);
    entry(&mut f, 1, "Catalog", 2, FREE);
    entry(&mut f, 2, "", 0, FREE);
    entry(&mut f, 3, "", 0, FREE);
    f
}

/// `desktop.ini` as Windows writes it: UTF-16LE with its byte-order mark.
pub fn desktop_ini_utf16() -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    for u in "[.ShellClassInfo]\r\nIconResource=C:\\x.ico,0\r\n".encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// `desktop.ini` as plain text.
pub fn desktop_ini_text() -> Vec<u8> {
    b"[.ShellClassInfo]\r\nIconResource=synthetic\r\n".to_vec()
}

/// el-2rpxq B3-R2: [`ds_store`] whose root node claims a child and a record
/// count that cannot exist (`0xFFFFFFFF` at file offsets 68 and 72).
pub fn ds_store_impossible_node() -> Vec<u8> {
    let mut f = ds_store();
    f[68..76].fill(0xFF);
    f
}

/// el-2rpxq B3-R2: [`thumbs_db`] whose `Catalog` stream claims one byte at
/// sector 999999, with neither a mini-FAT nor a mini-stream to hold it.
pub fn thumbs_db_missing_mini_stream() -> Vec<u8> {
    let mut f = thumbs_db();
    le32(&mut f, 1152 + 116, 999_999);
    f[1152 + 120..1152 + 128].copy_from_slice(&1u64.to_le_bytes());
    f
}
