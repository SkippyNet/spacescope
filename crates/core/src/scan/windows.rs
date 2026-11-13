//! Windows reader.
//!
//! `GetFileInformationByHandleEx(FileFullDirectoryInfo)` returns name, logical
//! size, allocated size, attributes and timestamps for hundreds of entries per
//! call, so no per-file query is ever made.

use super::{Platform, Scratch};
use crate::tree::{flags, push_wtf8};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED,
    ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileFullDirectoryInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

const BUF_WORDS: usize = 16 * 1024; // 128 KiB
const NAME_OFFSET: usize = 68;
/// Seconds between 1601-01-01 and 1970-01-01.
const EPOCH_DIFF: i64 = 11_644_473_600;

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Errors that mean "this filesystem cannot do the fast call".
pub(crate) fn should_fall_back(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION)
    )
}

/// Builds a NUL-terminated extended-length (`\\?\`) path.
fn verbatim(path: &Path, out: &mut Vec<u16>) {
    out.clear();
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    const BS: u16 = b'\\' as u16;
    let starts = |p: &[u16]| wide.len() >= p.len() && wide[..p.len()] == *p;
    let verb: [u16; 4] = [BS, BS, b'?' as u16, BS];
    if starts(&verb) {
        out.extend_from_slice(&wide);
    } else if starts(&[BS, BS]) {
        out.extend_from_slice(&verb);
        out.extend("UNC\\".encode_utf16());
        out.extend_from_slice(&wide[2..]);
    } else {
        out.extend_from_slice(&verb);
        out.extend_from_slice(&wide);
    }
    for c in out.iter_mut() {
        if *c == b'/' as u16 {
            *c = BS;
        }
    }
    out.push(0);
}

pub(crate) fn read_dir(path: &Path, s: &mut Scratch, _p: &Platform) -> io::Result<()> {
    let mut wide = std::mem::take(&mut s.wide);
    verbatim(path, &mut wide);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    s.wide = wide;
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = Handle(handle);

    if s.buf.len() < BUF_WORDS {
        s.buf.resize(BUF_WORDS, 0);
    }
    let buf_bytes = s.buf.len() * 8;
    let base = s.buf.as_mut_ptr() as *mut u8;

    loop {
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle.0,
                FileFullDirectoryInfo,
                base as *mut core::ffi::c_void,
                buf_bytes as u32,
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                return Ok(());
            }
            return Err(err);
        }

        let mut off = 0usize;
        loop {
            if off + NAME_OFFSET > buf_bytes {
                break;
            }
            // SAFETY: the kernel filled the buffer with FILE_FULL_DIR_INFO
            // records; every read below is bounds-checked against the buffer.
            let (next, mtime, size, alloc, attrs, name_bytes, reparse_tag) = unsafe {
                let p = base.add(off);
                (
                    (p as *const u32).read_unaligned() as usize,
                    (p.add(24) as *const i64).read_unaligned(),
                    (p.add(40) as *const i64).read_unaligned(),
                    (p.add(48) as *const i64).read_unaligned(),
                    (p.add(56) as *const u32).read_unaligned(),
                    (p.add(60) as *const u32).read_unaligned() as usize,
                    // EaSize doubles as the reparse tag for reparse points.
                    (p.add(64) as *const u32).read_unaligned(),
                )
            };
            if off + NAME_OFFSET + name_bytes > buf_bytes {
                break;
            }
            let name: &[u16] = unsafe {
                std::slice::from_raw_parts(
                    base.add(off + NAME_OFFSET) as *const u16,
                    name_bytes / 2,
                )
            };
            const DOT: u16 = b'.' as u16;
            if !(name == [DOT] || name == [DOT, DOT]) {
                let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                let mut fl = 0u8;
                if is_dir {
                    fl |= flags::DIR;
                }
                // Only name surrogates (junctions, symlinks) point elsewhere.
                // Other reparse directories (cloud, dedup, ...) hold real
                // content and are descended.
                if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 && reparse_tag & 0x2000_0000 != 0 {
                    fl |= flags::LINK;
                }
                let name_off = s.names.len();
                push_wtf8(&mut s.names, name);
                let name_len = (s.names.len() - name_off).min(u16::MAX as usize);
                s.names.truncate(name_off + name_len);
                s.entries.push(crate::tree::NewEntry {
                    name_off: name_off as u32,
                    name_len: name_len as u16,
                    flags: fl,
                    kind: 0,
                    size: if is_dir { 0 } else { size.max(0) as u64 },
                    alloc: if is_dir { 0 } else { alloc.max(0) as u64 },
                    mtime: mtime / 10_000_000 - EPOCH_DIFF,
                });
            }
            if next == 0 {
                break;
            }
            off += next;
        }
    }
}
