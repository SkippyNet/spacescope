//! Linux reader: raw `getdents64` with a large buffer, then one `fstatat`
//! per entry relative to the open directory (no path re-resolution).

use super::{Platform, Scratch};
use crate::tree::flags;
use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const BUF_WORDS: usize = 16 * 1024; // 128 KiB

struct Fd(libc::c_int);
impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

pub(crate) fn read_dir(path: &Path, s: &mut Scratch, p: &Platform) -> io::Result<()> {
    let cpath = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let fd = unsafe {
        libc::open(
            cpath.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOATIME,
        )
    };
    // O_NOATIME is refused for files we do not own; retry without it.
    let fd = if fd < 0 {
        unsafe {
            libc::open(
                cpath.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        }
    } else {
        fd
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = Fd(fd);

    if s.buf.len() < BUF_WORDS {
        s.buf.resize(BUF_WORDS, 0);
    }
    let buf_bytes = s.buf.len() * 8;
    let base = s.buf.as_mut_ptr() as *mut u8;

    loop {
        let n = unsafe { libc::syscall(libc::SYS_getdents64, fd.0, base, buf_bytes) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Ok(());
        }
        let n = n as usize;
        let mut off = 0usize;
        while off + 19 < n {
            // SAFETY: the kernel wrote `n` bytes of linux_dirent64 records:
            // u64 d_ino, i64 d_off, u16 d_reclen, u8 d_type, char d_name[].
            let (reclen, d_type, name_ptr) = unsafe {
                let rec = base.add(off);
                (
                    (rec.add(16) as *const u16).read_unaligned() as usize,
                    *rec.add(18),
                    rec.add(19) as *const libc::c_char,
                )
            };
            if reclen == 0 {
                break;
            }
            off += reclen;
            let name = unsafe { CStr::from_ptr(name_ptr) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }

            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::fstatat(
                    fd.0,
                    name_ptr,
                    &mut st,
                    libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
                )
            };
            if rc != 0 {
                let fl = if d_type == libc::DT_DIR {
                    flags::DIR | flags::ERROR
                } else {
                    0
                };
                s.push(name, fl, 0, 0, 0);
                continue;
            }

            let fmt = st.st_mode & libc::S_IFMT;
            let mut alloc = (st.st_blocks as u64).saturating_mul(512);
            let mtime = st.st_mtime as i64;
            if fmt == libc::S_IFDIR {
                let mut fl = flags::DIR;
                if st.st_dev as u64 != p.dev {
                    fl |= flags::MOUNT;
                    alloc = 0;
                }
                s.push(name, fl, 0, alloc, mtime);
            } else if fmt == libc::S_IFLNK {
                s.push(name, flags::LINK, st.st_size.max(0) as u64, alloc, mtime);
            } else if fmt == libc::S_IFREG {
                let mut fl = 0u8;
                let mut size = st.st_size.max(0) as u64;
                if st.st_nlink as u64 > 1 {
                    let first = p
                        .hardlinks
                        .lock()
                        .insert((st.st_dev as u64, st.st_ino as u64));
                    if !first {
                        fl |= flags::HARDLINK;
                        size = 0;
                        alloc = 0;
                    }
                }
                s.push(name, fl, size, alloc, mtime);
                if st.st_mode & 0o111 != 0 {
                    if let Some(e) = s.entries.last_mut() {
                        e.kind = crate::kind::PROGRAM;
                    }
                }
            } else {
                // Sockets, pipes, devices: present but weightless.
                s.push(name, 0, 0, alloc, mtime);
            }
        }
    }
}
