//! Portable reader built on `std::fs`. Used on platforms without a
//! specialised reader and as a fallback when the fast path is unsupported.

use super::{Platform, Scratch};
use crate::tree::flags;
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[allow(dead_code)]
pub(crate) fn read_dir(path: &Path, s: &mut Scratch, _p: &Platform) -> io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Ok(md) = entry.metadata() else {
            s.push(name.as_encoded_bytes(), flags::ERROR, 0, 0, 0);
            continue;
        };
        let ft = md.file_type();
        let mut fl = 0u8;
        if ft.is_dir() {
            fl |= flags::DIR;
        }
        if ft.is_symlink() {
            fl |= flags::LINK;
            #[cfg(windows)]
            {
                use std::os::windows::fs::FileTypeExt;
                if ft.is_symlink_dir() {
                    fl |= flags::DIR;
                }
            }
        }
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        let is_dir = fl & flags::DIR != 0;
        let size = if is_dir { 0 } else { md.len() };
        #[cfg(unix)]
        let alloc = {
            use std::os::unix::fs::MetadataExt;
            md.blocks() * 512
        };
        #[cfg(not(unix))]
        let alloc = size;
        s.push(name.as_encoded_bytes(), fl, size, alloc, mtime);
    }
    Ok(())
}
