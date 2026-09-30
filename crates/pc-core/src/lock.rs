//! One writer for one archive, across processes.
//!
//! The server keeps a single writing job at a time, and that gate lives in
//! its own memory. It says nothing about the command line, or about a second
//! server started on the same database — and those write to the same index
//! and move the same files. A plan reviewed by one of them can be carried out
//! while the other is halfway through rearranging the archive underneath it.
//!
//! So the gate is a lock the operating system keeps: an exclusive advisory
//! lock on a file beside the database, taken for as long as the writing
//! lasts. It is released when the handle closes, which includes the process
//! being killed — nothing has to be cleaned up afterwards, and a crash cannot
//! leave the archive locked.
//!
//! Reading is not gated. Listing groups, opening a frame or watching a job
//! goes on while someone writes, as it always did; SQLite handles that.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Someone else is writing to this archive.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Busy(pub String);

/// Held for as long as the writing lasts. Dropping it lets the next one in.
#[derive(Debug)]
pub struct WriterLock {
    _file: File,
}

/// The file the lock lives on: beside the database, never inside the archive.
pub fn lock_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.file_name().unwrap_or_default().to_os_string();
    name.push(".writer-lock");
    db_path.with_file_name(name)
}

/// Become the writer for this database, or say who already is.
///
/// `what` is what this process is about to do, written into the file so the
/// refusal can be specific rather than "busy".
pub fn take_writer(db_path: &Path, what: &str) -> Result<WriterLock> {
    let path = lock_path(db_path);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    // The note below is written into whatever the name leads to. A link
    // there — a symbolic link, or a second name of somebody else's file —
    // would have that file truncated and overwritten, so the name is never
    // followed, and a file with other names is refused before a byte of it
    // changes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&path).with_context(|| {
        crate::tf!(
            "не открыть файл замка {0} (он должен быть обычным файлом, не ссылкой)",
            "cannot open the lock file {0} (it must be a plain file, not a link)",
            path.display()
        )
    })?;
    lone_file(&file).with_context(|| {
        crate::tf!(
            "файл замка {0} не тронут",
            "the lock file {0} was left untouched",
            path.display()
        )
    })?;

    if !try_lock(&file)? {
        let held = note_of(&path);
        let held = held.trim();
        return Err(Busy(if held.is_empty() {
            crate::tr!(
                "с этим архивом уже работает другой процесс",
                "another process is already working on this archive"
            )
            .into()
        } else {
            crate::tf!(
                "с этим архивом уже работает другой процесс: {0}",
                "another process is already working on this archive: {0}",
                held
            )
        })
        .into());
    }

    // Whoever is refused next reads this line, so it says who and what.
    let note = format!(
        "{what}; pid {}; {}",
        std::process::id(),
        crate::time::now_unix()
    );
    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(NOTE_AT));
    let _ = file.write_all(note.as_bytes());
    let _ = file.flush();
    Ok(WriterLock { _file: file })
}

/// The lock file is a plain file with this one name, so writing the note
/// cannot change anything else.
#[cfg(unix)]
fn lone_file(file: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    if !m.is_file() {
        anyhow::bail!(crate::tr!("это не обычный файл", "it is not a plain file"));
    }
    if m.nlink() != 1 {
        anyhow::bail!(crate::tf!(
            "у этого файла {0} имени: запись изменила бы и другие",
            "this file has {0} names: writing to it would change the others too",
            m.nlink()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn lone_file(file: &File) -> Result<()> {
    if !file.metadata()?.is_file() {
        anyhow::bail!(crate::tr!("это не обычный файл", "it is not a plain file"));
    }
    Ok(())
}

/// The first byte is the lock itself; the note lives after it.
///
/// Windows locks a range of bytes against reading as well, so a note inside
/// the locked range could not be read by the process being refused — which is
/// the only process that ever needs it. One byte is locked, and the words go
/// past it, where any reader may look.
const NOTE_AT: u64 = 1;

fn note_of(path: &Path) -> String {
    let mut held = String::new();
    let _ = File::open(path).and_then(|mut f| {
        f.seek(SeekFrom::Start(NOTE_AT))?;
        f.read_to_string(&mut held)
    });
    held
}

#[cfg(unix)]
fn try_lock(file: &File) -> Result<bool> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: the descriptor is owned by `file` and outlives the call.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EINTR => Ok(false),
        _ => Err(err).context(crate::tr!(
            "не взять замок на запись",
            "cannot take the writing lock"
        )),
    }
}

#[cfg(windows)]
fn try_lock(file: &File) -> Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    let mut overlapped = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is owned by `file`; the overlapped struct is zeroed
    // and lives for the whole call, which does not return until it is done.
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle() as _,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            // One byte, at offset zero: see `NOTE_AT`.
            1,
            0,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == ERROR_LOCK_VIOLATION as i32 => Ok(false),
        _ => Err(err).context(crate::tr!(
            "не взять замок на запись",
            "cannot take the writing lock"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_writer_is_refused_and_told_who_is_working() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("test.db");
        let held = take_writer(&db, "раскладка по датам").unwrap();

        let refused = take_writer(&db, "перенос копий").unwrap_err();
        assert!(refused.is::<Busy>(), "{refused:#}");
        let said = format!("{refused:#}");
        assert!(said.contains("раскладка по датам"), "{said}");
        assert!(said.contains(&std::process::id().to_string()), "{said}");

        // And the next one in gets it.
        drop(held);
        let mine = take_writer(&db, "перенос копий");
        assert!(mine.is_ok(), "{:#}", mine.unwrap_err());
    }

    #[test]
    fn the_holders_note_can_be_read_while_the_lock_is_held() {
        // Windows locks a byte range against reading too, so the note has to
        // live outside the range the lock uses — otherwise the only process
        // that needs to read it is the one process that cannot.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("test.db");
        let _held = take_writer(&db, "перенос копий").unwrap();
        assert!(note_of(&lock_path(&db)).contains("перенос копий"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_the_lock_name_is_refused_and_its_file_keeps_every_byte() {
        // Somebody else's file behind a symbolic link or a second name at
        // the lock's name would be truncated and overwritten by the note.
        for hard in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let db = tmp.path().join("photo-cleanup.db");
            let foreign = tmp.path().join("foreign-notes.txt");
            std::fs::write(&foreign, b"foreign original 97301").unwrap();
            let before = std::fs::metadata(&foreign).unwrap().modified().unwrap();
            if hard {
                std::fs::hard_link(&foreign, lock_path(&db)).unwrap();
            } else {
                std::os::unix::fs::symlink(&foreign, lock_path(&db)).unwrap();
            }
            for _ in 0..2 {
                let refused = take_writer(&db, "перенос копий").unwrap_err();
                assert!(!refused.is::<Busy>(), "{refused:#}");
                assert!(
                    format!("{refused:#}").contains("writer-lock"),
                    "the refusal names the lock file: {refused:#}"
                );
            }
            assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign original 97301");
            assert_eq!(
                std::fs::metadata(&foreign).unwrap().modified().unwrap(),
                before
            );
            assert!(
                std::fs::symlink_metadata(lock_path(&db)).is_ok(),
                "left in place"
            );
        }
    }

    #[test]
    fn the_lock_file_sits_beside_the_database_not_in_the_archive() {
        let db = Path::new("/data/pc.db");
        assert_eq!(lock_path(db), Path::new("/data/pc.db.writer-lock"));
    }
}
