//! Windows file system calls through handles: the one place they live.
//!
//! A path names a place, a handle names an object. Everything a move needs to
//! be sure of — which object is renamed, into which folder, what it is and
//! whether it is still the one that was checked — is asked of handles here,
//! never of a path resolved again. These were written for the relocation of
//! the data folder (pc-desktop, el-2yxx0) and are shared from here so the
//! photo moves (`anchored`) use the same, natively tested calls (el-543ou).
//!
//! Nothing here deletes. There is no removal primitive on Windows in this
//! crate, so a removal path cannot be assembled on Windows even by mistake.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::PathBuf;
use std::{fs, io, mem, ptr};

pub use crate::proof::Kind;
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FileRenameInformation, NtCreateFile, NtSetInformationFile, FILE_RENAME_INFORMATION,
    FILE_RENAME_INFORMATION_0,
};
use windows_sys::Win32::Foundation::{
    RtlNtStatusToDosError, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS,
    UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, FileBasicInfo, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
    FileIdInfo, FileIdType, FileStandardInfo, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, OpenFileById,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
    FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_BOTH_DIR_INFO, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_ID_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, SYNCHRONIZE, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// `IO_REPARSE_TAG_SYMLINK` and `IO_REPARSE_TAG_MOUNT_POINT` (a junction):
/// the two reparse points that are links. Any other tag (cloud files,
/// deduplication, WOF, app execution aliases) is an object this tool does not
/// understand.
const TAG_SYMLINK: u32 = 0xA000_000C;
const TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// The one file system whose file IDs were verified natively to stay with an
/// object across renames and not to be handed out again (el-61ar8 P1, P5).
/// ReFS (128-bit IDs), FAT and exFAT (no `FileIdInfo` at all) are refused
/// rather than given a weaker proof (director decision on el-61ar8 §5).
const SUPPORTED_FILE_SYSTEM: &str = "NTFS";

fn nt_error(status: NTSTATUS) -> io::Error {
    // SAFETY: no preconditions.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}

fn info<T>(file: &fs::File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    // SAFETY: plain data, filled by the call.
    let mut out: T = unsafe { mem::zeroed() };
    // SAFETY: `out` is writable for its whole size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            class,
            (&raw mut out).cast(),
            mem::size_of::<T>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(out)
}

/// Open `name` — one entry, never a path — relative to the directory open as
/// `dir`, with `NtCreateFile` and `RootDirectory`. The caller checks the name
/// first: NT itself accepts `sub\x` here and walks through a junction in the
/// middle of it (el-61ar8 P2g, P6c). No `OBJ_CASE_INSENSITIVE`: in a folder
/// marked case-sensitive the name means exactly this spelling.
pub(crate) fn open_at(
    dir: &fs::File,
    name: &OsStr,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
) -> io::Result<fs::File> {
    use windows_sys::Wdk::Storage::FileSystem::FILE_SYNCHRONOUS_IO_NONALERT;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    let wide: Vec<u16> = name.encode_wide().collect();
    let bytes = u16::try_from(wide.len() * 2)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name too long"))?;
    let object = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: wide.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: dir.as_raw_handle() as HANDLE,
        ObjectName: &object,
        Attributes: 0,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle: HANDLE = ptr::null_mut();
    // SAFETY: plain data, written by the call.
    let mut status_block: IO_STATUS_BLOCK = unsafe { mem::zeroed() };
    // SAFETY: every pointer lives for the call; a returned handle is new
    // and owned by the `File` below.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            share,
            disposition,
            options | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    // SAFETY: a fresh handle nobody else owns.
    Ok(unsafe { fs::File::from_raw_handle(handle.cast()) })
}

/// Rename the object open as `entry` (with `DELETE` access) to `name`
/// in the directory open as `to`, through the handles only
/// (`NtSetInformationFile(FileRenameInformation)` with
/// `RootDirectory`): no path is resolved, and an existing entry at
/// `name` fails the call and is kept. Not `SetFileInformationByHandle`:
/// it turns a relative name into an absolute path under the current
/// directory first, which with a `RootDirectory` is os error 87 (seen
/// natively on the CI runner, el-2yxx0).
pub fn rename_into(entry: &fs::File, to: &fs::File, name: &OsStr) -> io::Result<()> {
    let wide: Vec<u16> = name.encode_wide().collect();
    let header = mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
    let bytes = (header + wide.len() * 2).max(mem::size_of::<FILE_RENAME_INFORMATION>());
    let mut buf = vec![0u64; bytes.div_ceil(8)];
    let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `buf` is 8-byte aligned, zeroed and holds the header plus
    // the name; the fields are written in place.
    unsafe {
        (*info).Anonymous = FILE_RENAME_INFORMATION_0 { ReplaceIfExists: 0 };
        (*info).RootDirectory = to.as_raw_handle() as HANDLE;
        (*info).FileNameLength = (wide.len() * 2) as u32;
        ptr::copy_nonoverlapping(
            wide.as_ptr(),
            (&raw mut (*info).FileName).cast::<u16>(),
            wide.len(),
        );
    }
    // SAFETY: plain data, written by the call.
    let mut status_block: IO_STATUS_BLOCK = unsafe { mem::zeroed() };
    // SAFETY: `entry` is open with DELETE, `to` is an open directory;
    // `buf` and `status_block` live for the call.
    let status = unsafe {
        NtSetInformationFile(
            entry.as_raw_handle() as HANDLE,
            &mut status_block,
            buf.as_ptr().cast(),
            bytes as u32,
            FileRenameInformation,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    Ok(())
}

/// Where the object open as `file` is now, as the system names it
/// (`\\?\C:\…` or `\\?\UNC\…`).
pub fn final_path(file: &fs::File) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let handle = file.as_raw_handle() as HANDLE;
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: `buf` is writable for its whole length.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buf.as_mut_ptr(),
                buf.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return Some(std::ffi::OsString::from_wide(&buf[..n]).into());
        }
        buf.resize(n + 1, 0);
    }
}

/// Open the entry with file ID `id` on the volume of `volume`, as
/// itself (no reparse point is followed).
pub fn open_by_id(volume: &fs::File, id: i64, access: u32) -> io::Result<fs::File> {
    let descriptor = FILE_ID_DESCRIPTOR {
        dwSize: mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: id },
    };
    // SAFETY: `volume` is an open handle, `descriptor` lives for the
    // call; a returned handle is new and owned by the `File` below.
    let child = unsafe {
        OpenFileById(
            volume.as_raw_handle() as HANDLE,
            &descriptor,
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if child == INVALID_HANDLE_VALUE || child.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle nobody else owns.
    Ok(unsafe { fs::File::from_raw_handle(child.cast()) })
}

/// The entry `name` in the directory open as `dir`, found by listing the
/// handle and opened by file ID as itself (no reparse point followed),
/// for its identity only: without `DELETE`, so it never conflicts with
/// another handle's sharing.
pub fn child(dir: &fs::File, name: &OsStr) -> io::Result<fs::File> {
    let wanted: Vec<u16> = name.encode_wide().collect();
    let id = children(dir)?
        .into_iter()
        .find(|(entry, _, _)| *entry == wanted)
        .map(|(_, id, _)| id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not in the folder", name.to_string_lossy()),
            )
        })?;
    open_by_id(dir, id, FILE_READ_ATTRIBUTES | SYNCHRONIZE)
}

/// Names, 64-bit file IDs and attributes of the entries in the directory
/// open as `dir`, without `.`/`..`. Lists the handle, not a path. The ID is
/// the short form `FileIdBothDirectoryInfo` reports; it is enough to reopen
/// an entry on NTFS and is not what identifies an object (see [`ident`]).
pub fn children(dir: &fs::File) -> io::Result<Vec<(Vec<u16>, i64, u32)>> {
    let handle = dir.as_raw_handle() as HANDLE;
    let mut buf = [0u64; 512];
    let mut class = FileIdBothDirectoryRestartInfo;
    let mut ids = Vec::new();
    loop {
        // SAFETY: `buf` is writable for its whole size.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                class,
                buf.as_mut_ptr().cast(),
                mem::size_of_val(&buf) as u32,
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                Ok(ids)
            } else {
                Err(e)
            };
        }
        class = FileIdBothDirectoryInfo;
        let mut offset = 0usize;
        loop {
            // SAFETY: the call filled `buf` with a chain of entries;
            // `NextEntryOffset` stays within it.
            let (name, id, attributes, next) = unsafe {
                let entry = buf
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset)
                    .cast::<FILE_ID_BOTH_DIR_INFO>();
                let len = (*entry).FileNameLength as usize / 2;
                let name =
                    std::slice::from_raw_parts((&raw const (*entry).FileName).cast::<u16>(), len);
                (
                    name,
                    (*entry).FileId,
                    (*entry).FileAttributes,
                    (*entry).NextEntryOffset,
                )
            };
            if name != [u16::from(b'.')] && name != [u16::from(b'.'), u16::from(b'.')] {
                ids.push((name.to_vec(), id, attributes));
            }
            if next == 0 {
                break;
            }
            offset += next as usize;
        }
    }
}

/// The name of the file system the object open as `file` is on.
pub fn file_system(file: &fs::File) -> io::Result<String> {
    let mut name = [0u16; 64];
    // SAFETY: `name` is writable for its whole length; the other buffers
    // are not asked for.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle() as HANDLE,
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Ok(String::from_utf16_lossy(&name[..len]))
}

fn unsupported(fs_name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "the file system {fs_name} is not supported: only on NTFS is a file's identity \
             known to survive a move"
        ),
    )
}

/// A query that failed on a file system other than NTFS (FAT and exFAT do
/// not answer some of them) is that file system's refusal, said as such.
fn unsupported_or(file: &fs::File, e: io::Error) -> io::Error {
    match file_system(file) {
        Ok(name) if name != SUPPORTED_FILE_SYSTEM => unsupported(&name),
        _ => e,
    }
}

/// What identifies the object open as `file`: the volume serial number and
/// the NTFS file ID, both from `FileIdInfo`, which a rename keeps (el-61ar8
/// P1) and which 300 create/delete cycles never handed out twice (P5).
///
/// Refused, never weakened: a file system other than NTFS; FAT and exFAT,
/// which have no `FileIdInfo` (F2); and an ID whose upper 64 bits are set
/// (ReFS), which would not fit the evidence written today. Not the
/// `nFileIndex` of `GetFileInformationByHandle`: on FAT it is not stable.
pub fn ident(file: &fs::File) -> io::Result<(u64, u64)> {
    let fs_name = file_system(file)?;
    if fs_name != SUPPORTED_FILE_SYSTEM {
        return Err(unsupported(&fs_name));
    }
    let id: FILE_ID_INFO = info(file, FileIdInfo)?;
    let bytes = id.FileId.Identifier;
    let (low, high) = bytes.split_at(8);
    if high.iter().any(|&b| b != 0) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this file system names files with 128-bit IDs, which are not supported",
        ));
    }
    let mut low8 = [0u8; 8];
    low8.copy_from_slice(low);
    Ok((id.VolumeSerialNumber, u64::from_le_bytes(low8)))
}

/// `FileStandardInfo` of the object open as `file`: size, number of names,
/// whether its deletion is pending, whether it is a directory.
pub fn standard(file: &fs::File) -> io::Result<FILE_STANDARD_INFO> {
    info(file, FileStandardInfo)
}

/// `FileBasicInfo`: the times (100 ns since 1601) and attributes.
pub fn basic(file: &fs::File) -> io::Result<FILE_BASIC_INFO> {
    info(file, FileBasicInfo)
}

/// The kind of the object open as `file`, as itself, from its attributes and
/// reparse tag (`FileAttributeTagInfo`), and the attributes.
///
/// [`Kind::Link`] is a symbolic link or a junction. [`Kind::Other`] is
/// anything else this tool does not act on: a reparse point of another kind
/// (OneDrive and other cloud placeholders, deduplicated or WOF-compressed
/// files) or a file whose data is not here (offline, recalled on access).
pub fn kind(file: &fs::File) -> io::Result<(Kind, u32)> {
    let tag: FILE_ATTRIBUTE_TAG_INFO =
        info(file, FileAttributeTagInfo).map_err(|e| unsupported_or(file, e))?;
    let a = tag.FileAttributes;
    let kind = if a & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        if matches!(tag.ReparseTag, TAG_SYMLINK | TAG_MOUNT_POINT) {
            Kind::Link
        } else {
            Kind::Other
        }
    } else if a
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
    {
        Kind::Other
    } else if a & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Dir
    } else {
        Kind::File
    };
    Ok((kind, a))
}

/// A Windows file time (100 ns since 1601) as nanoseconds since 1970.
pub fn unix_nanos(filetime: i64) -> i128 {
    const EPOCH_DIFF_100NS: i128 = 116_444_736_000_000_000;
    (filetime as i128 - EPOCH_DIFF_100NS) * 100
}

/// `\\?\C:\x` → `C:\x`, `\\?\UNC\server\share\x` → `\\server\share\x`: the
/// form a path recorded from a walk has, so the two can be compared.
pub fn without_verbatim(path: PathBuf) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    // Wide, not `to_string_lossy`: a name Windows allows but UTF-8 cannot
    // carry must come back unchanged.
    if let Some(rest) = wide.strip_prefix(w(r"\\?\UNC\").as_slice()) {
        let mut out = w(r"\\");
        out.extend_from_slice(rest);
        return std::ffi::OsString::from_wide(&out).into();
    }
    if let Some(rest) = wide.strip_prefix(w(r"\\?\").as_slice()) {
        // Only a drive path loses its prefix; `\\?\Volume{…}\` has no other
        // spelling and stays as it is.
        if rest.get(1) == Some(&u16::from(b':')) {
            return std::ffi::OsString::from_wide(rest).into();
        }
    }
    path
}
