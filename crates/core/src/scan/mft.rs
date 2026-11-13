//! NTFS fast path: read the Master File Table straight off the volume.
//!
//! Every file and folder on an NTFS volume has a 1 KiB record in `$MFT`
//! holding its name, parent, sizes and timestamps. Reading that one file
//! sequentially replaces hundreds of thousands of directory opens, and it
//! sees everything regardless of permissions. Opening a volume needs
//! administrator rights; without them (or on any surprise) the caller falls
//! back to the ordinary directory walk.

use super::Progress;
use crate::kind;
use crate::tree::{flags, push_wtf8, NewEntry, NodeId, Shared, Tree};
use std::collections::VecDeque;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, GetVolumeInformationW, ReadFile, SetFilePointerEx,
    BY_HANDLE_FILE_INFORMATION, FILE_BEGIN, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x0009_0064;
const FSCTL_GET_RETRIEVAL_POINTERS: u32 = 0x0009_0073;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const ROOT_RECORD: u64 = 5;
const BAD_CLUSTER_RECORD: u64 = 8;
const PIECE_BYTES: u64 = 4 << 20;
/// Seconds between 1601-01-01 and 1970-01-01.
const EPOCH_DIFF: i64 = 11_644_473_600;

const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_INDEX_ALLOCATION: u32 = 0xA0;
const ATTR_REPARSE_POINT: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
// A raw handle is just a number; each thread uses its own.
unsafe impl Send for Handle {}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn bad(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn open(path: &[u16], access: u32, flags: u32) -> io::Result<Handle> {
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            SHARE_ALL,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(h))
    }
}

/// The drive letter of a plain `X:\...` path.
pub(crate) fn drive_of(path: &Path) -> Option<char> {
    let s = path.to_str()?;
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\')
        .then(|| b[0].to_ascii_uppercase() as char)
}

fn is_ntfs(letter: char) -> bool {
    let root = wide(&format!("{letter}:\\"));
    let mut fs = [0u16; 32];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    ok != 0 && String::from_utf16_lossy(&fs).trim_end_matches('\0') == "NTFS"
}

fn read_at(h: &Handle, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    if unsafe { SetFilePointerEx(h.0, offset as i64, std::ptr::null_mut(), FILE_BEGIN) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut done = 0usize;
    while done < buf.len() {
        let mut got = 0u32;
        let ok = unsafe {
            ReadFile(
                h.0,
                buf[done..].as_mut_ptr(),
                (buf.len() - done) as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if got == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short volume read",
            ));
        }
        done += got as usize;
    }
    Ok(())
}

#[inline]
fn u16_at(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes([b[o], b[o + 1]]) as usize
}
#[inline]
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

struct Volume {
    cluster: u64,
    record: usize,
    mft_valid: u64,
    mft_lcn: u64,
    total_bytes: u64,
}

fn volume_data(h: &Handle) -> io::Result<Volume> {
    let mut out = [0u64; 16];
    let mut ret = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h.0,
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            out.as_mut_ptr() as *mut _,
            std::mem::size_of_val(&out) as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // NTFS_VOLUME_DATA_BUFFER, as 64-bit words.
    let cluster = out[5] >> 32;
    let record = (out[6] & 0xFFFF_FFFF) as usize;
    let v = Volume {
        cluster,
        record,
        mft_valid: out[7],
        mft_lcn: out[8],
        total_bytes: out[2].saturating_mul(cluster),
    };
    // The layout this reader understands: whole records inside clusters.
    if !(512..=65536).contains(&v.record)
        || !v.record.is_power_of_two()
        || v.cluster < v.record as u64
        || !v.cluster.is_multiple_of(v.record as u64)
    {
        return Err(bad("unsupported NTFS geometry"));
    }
    Ok(v)
}

/// `(vcn, lcn, clusters)` runs of `$MFT`, asked from the filesystem itself so
/// that heavily fragmented tables (attribute lists) need no special handling.
fn mft_extents_from_fs(letter: char) -> io::Result<Vec<(u64, u64, u64)>> {
    let h = open(
        &wide(&format!("\\\\.\\{letter}:\\$MFT")),
        FILE_READ_ATTRIBUTES,
        0,
    )?;
    let mut runs = Vec::new();
    let mut start: i64 = 0;
    let mut out = vec![0u64; 2 + 2 * 512];
    loop {
        let mut ret = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                FSCTL_GET_RETRIEVAL_POINTERS,
                &start as *const i64 as *const _,
                8,
                out.as_mut_ptr() as *mut _,
                (out.len() * 8) as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        let more = if ok != 0 {
            false
        } else {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Err(e);
            }
            true
        };
        let count = ((out[0] & 0xFFFF_FFFF) as usize).min(512);
        let mut prev = out[1] as i64;
        for i in 0..count {
            let next = out[2 + 2 * i] as i64;
            let lcn = out[3 + 2 * i] as i64;
            if next <= prev {
                return Err(bad("bad retrieval pointers"));
            }
            if lcn >= 0 {
                runs.push((prev as u64, lcn as u64, (next - prev) as u64));
            }
            prev = next;
        }
        if !more || count == 0 {
            return Ok(runs);
        }
        start = prev;
    }
}

/// Fallback: decode the run list of `$MFT`'s own record.
fn mft_extents_from_record0(vol: &Volume, volume: &Handle) -> io::Result<Vec<(u64, u64, u64)>> {
    let mut rec = vec![0u8; vol.record.max(vol.cluster as usize)];
    read_at(volume, vol.mft_lcn * vol.cluster, &mut rec)?;
    let rec = &mut rec[..vol.record];
    if !fixup(rec) {
        return Err(bad("$MFT record is damaged"));
    }
    let used = (u32_at(rec, 0x18) as usize).min(rec.len());
    let mut off = u16_at(rec, 0x14);
    while off + 16 <= used {
        let ty = u32_at(rec, off);
        let len = u32_at(rec, off + 4) as usize;
        if ty == ATTR_END || len < 16 || off + len > used {
            break;
        }
        if ty == ATTR_DATA && rec[off + 8] != 0 && rec[off + 9] == 0 && len >= 64 {
            let mut p = off + u16_at(rec, off + 32);
            let end = off + len;
            let (mut vcn, mut lcn) = (u64_at(rec, off + 16), 0i64);
            let mut runs = Vec::new();
            while p < end && rec[p] != 0 {
                let (ls, os) = ((rec[p] & 0x0F) as usize, (rec[p] >> 4) as usize);
                p += 1;
                if ls == 0 || ls > 8 || os > 8 || p + ls + os > end {
                    return Err(bad("bad $MFT run list"));
                }
                let mut n = 0u64;
                for i in 0..ls {
                    n |= (rec[p + i] as u64) << (8 * i);
                }
                p += ls;
                if os > 0 {
                    let mut d = 0i64;
                    for i in 0..os {
                        d |= (rec[p + i] as i64) << (8 * i);
                    }
                    // Sign-extend the delta.
                    let shift = 64 - 8 * os as u32;
                    lcn += (d << shift) >> shift;
                    p += os;
                    runs.push((vcn, lcn as u64, n));
                }
                vcn += n;
            }
            return Ok(runs);
        }
        off += len;
    }
    Err(bad("$MFT has no data runs"))
}

/// Applies the update sequence array. Returns false for a torn record.
fn fixup(rec: &mut [u8]) -> bool {
    let (uo, uc) = (u16_at(rec, 4), u16_at(rec, 6));
    if uc < 2 {
        return true;
    }
    let stride = rec.len() / (uc - 1);
    if stride < 2 || uo + 2 * uc > rec.len() {
        return false;
    }
    let sig = [rec[uo], rec[uo + 1]];
    for i in 1..uc {
        let end = i * stride;
        if end > rec.len() {
            break;
        }
        if rec[end - 2..end] != sig {
            return false;
        }
        rec[end - 2] = rec[uo + 2 * i];
        rec[end - 1] = rec[uo + 2 * i + 1];
    }
    true
}

const R_USED: u8 = 1;
const R_DIR: u8 = 1 << 1;
const R_LINK: u8 = 1 << 2;
/// The record carries the unnamed data stream's size itself.
const R_SIZED: u8 = 1 << 3;

#[derive(Clone, Copy, Default)]
struct Rec {
    size: u64,
    alloc: u64,
    mtime: i64,
    parent: u32,
    name_off: u32,
    name_len: u16,
    worker: u8,
    flags: u8,
}

/// Sizes found in an extension record, owed to its base record.
struct Owed {
    base: u32,
    size: u64,
    alloc: u64,
    sized: bool,
}

#[derive(Default)]
struct Part {
    recs: Vec<(u32, Rec)>,
    owed: Vec<Owed>,
    names: Vec<u8>,
}

fn parse_record(rec: &mut [u8], recno: u64, worker: u8, vol_bytes: u64, out: &mut Part) {
    if rec.len() < 0x30 || &rec[..4] != b"FILE" || recno > u32::MAX as u64 || !fixup(rec) {
        return;
    }
    let rflags = u16_at(rec, 0x16);
    if rflags & 1 == 0 {
        return; // not in use
    }
    let base = u64_at(rec, 0x20) & REF_MASK;
    let used = (u32_at(rec, 0x18) as usize).min(rec.len());

    let (mut size, mut alloc, mut sized) = (0u64, 0u64, false);
    let (mut mtime, mut attrs, mut tag) = (0i64, 0u32, 0u32);
    // (rank, parent, name offset, name length in UTF-16 units)
    let mut best: Option<(u8, u64, usize, usize)> = None;

    let mut off = u16_at(rec, 0x14);
    while off + 16 <= used {
        let ty = u32_at(rec, off);
        if ty == ATTR_END {
            break;
        }
        let len = u32_at(rec, off + 4) as usize;
        if len < 16 || off + len > used {
            break;
        }
        let end = off + len;
        let resident = rec[off + 8] == 0;
        let named = rec[off + 9] != 0;
        // Start of a resident attribute's value.
        let value = if resident && len >= 24 {
            Some(off + u16_at(rec, off + 20))
        } else {
            None
        };
        match (ty, value) {
            (ATTR_STANDARD_INFORMATION, Some(c)) if c + 36 <= end => {
                mtime = (u64_at(rec, c + 8) / 10_000_000) as i64 - EPOCH_DIFF;
                attrs = u32_at(rec, c + 32);
            }
            (ATTR_FILE_NAME, Some(c)) if c + 66 <= end => {
                let n = rec[c + 64] as usize;
                // Prefer the long name; 2 is the 8.3 DOS alias.
                let rank = (rec[c + 65] != 2) as u8;
                if c + 66 + 2 * n <= end && best.is_none_or(|b| rank > b.0) {
                    best = Some((rank, u64_at(rec, c) & REF_MASK, c + 66, n));
                }
            }
            (ATTR_REPARSE_POINT, Some(c)) if c + 4 <= end => tag = u32_at(rec, c),
            (ATTR_DATA, Some(_)) => {
                if !named {
                    size = u32_at(rec, off + 16) as u64;
                    sized = true;
                }
            }
            (ATTR_DATA | ATTR_INDEX_ALLOCATION, None) if len >= 64 => {
                // Only the first piece of a split attribute carries sizes.
                if u64_at(rec, off + 16) == 0 {
                    let packed = u16_at(rec, off + 34) != 0 && u16_at(rec, off + 12) & 0x8001 != 0;
                    let mut on_disk = if packed && len >= 72 {
                        u64_at(rec, off + 64)
                    } else {
                        u64_at(rec, off + 40)
                    };
                    if on_disk > vol_bytes {
                        on_disk = 0;
                    }
                    if ty == ATTR_DATA && !named {
                        size = u64_at(rec, off + 48);
                        sized = true;
                        alloc += on_disk;
                    } else if recno != BAD_CLUSTER_RECORD {
                        // Named streams and directory indexes use real space.
                        alloc += on_disk;
                    }
                }
            }
            _ => {}
        }
        off = end;
    }

    if base != 0 {
        if base <= u32::MAX as u64 && (alloc > 0 || sized) {
            out.owed.push(Owed {
                base: base as u32,
                size,
                alloc,
                sized,
            });
        }
        return;
    }
    let Some((_, parent, name_at, n)) = best else {
        return;
    };
    if parent > u32::MAX as u64 {
        return;
    }
    let mut name = [0u16; 255];
    for (i, w) in name[..n].iter_mut().enumerate() {
        *w = u16::from_le_bytes([rec[name_at + 2 * i], rec[name_at + 2 * i + 1]]);
    }
    let name_off = out.names.len();
    push_wtf8(&mut out.names, &name[..n]);
    let is_dir = rflags & 2 != 0;
    let mut f = R_USED;
    if is_dir {
        f |= R_DIR;
        size = 0;
    }
    if sized {
        f |= R_SIZED;
    }
    // Name surrogates (junctions, symlinks) point somewhere else.
    if attrs & 0x400 != 0 && tag & 0x2000_0000 != 0 {
        f |= R_LINK;
    }
    out.recs.push((
        recno as u32,
        Rec {
            size,
            alloc,
            mtime,
            parent: parent as u32,
            name_off: name_off as u32,
            name_len: (out.names.len() - name_off) as u16,
            worker,
            flags: f,
        },
    ));
}

struct Piece {
    offset: u64,
    first: u64,
    count: usize,
}

/// MFT record number of a directory, from its file id.
fn record_of(path: &Path) -> io::Result<u64> {
    let mut w: Vec<u16> = "\\\\?\\".encode_utf16().collect();
    w.extend(path.as_os_str().encode_wide());
    w.push(0);
    let h = open(&w, FILE_READ_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h.0, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64) & REF_MASK)
}

/// Builds the whole tree for `root` from the MFT and installs it in `shared`.
/// Any error leaves `shared` untouched so the caller can fall back.
pub(crate) fn scan(shared: &Shared, root: &Path, progress: &Progress) -> io::Result<()> {
    let clock = std::time::Instant::now();
    let timing = std::env::var_os("SSCAN_TIMING").is_some();
    let lap = |what: &str| {
        if timing {
            eprintln!("mft: {what} at {:.3} s", clock.elapsed().as_secs_f64());
        }
    };
    let letter = drive_of(root).ok_or_else(|| bad("not a drive path"))?;
    if !is_ntfs(letter) {
        return Err(bad("not NTFS"));
    }
    let volume_path = wide(&format!("\\\\.\\{letter}:"));
    let volume = open(&volume_path, GENERIC_READ, 0)?;
    let vol = volume_data(&volume)?;
    let total = vol.mft_valid / vol.record as u64;
    if total == 0 || total > u32::MAX as u64 {
        return Err(bad("unexpected MFT size"));
    }
    let root_rec = if root.parent().is_none() {
        ROOT_RECORD
    } else {
        record_of(root)?
    };
    if root_rec >= total {
        return Err(bad("root record out of range"));
    }
    let extents = match mft_extents_from_fs(letter) {
        Ok(e) if !e.is_empty() => e,
        _ => mft_extents_from_record0(&vol, &volume)?,
    };

    // Cut the table into pieces the workers read independently.
    let per_cluster = vol.cluster / vol.record as u64;
    let mut pieces = Vec::new();
    for &(vcn, lcn, clusters) in &extents {
        let step = (PIECE_BYTES / vol.cluster).max(1);
        let mut done = 0;
        while done < clusters {
            let n = step.min(clusters - done);
            let first = (vcn + done) * per_cluster;
            if first < total {
                pieces.push(Piece {
                    offset: (lcn + done) * vol.cluster,
                    first,
                    count: (n * per_cluster).min(total - first) as usize,
                });
            }
            done += n;
        }
    }

    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 8);
    let parts: Vec<io::Result<Part>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|w| {
                let (pieces, next, vol, volume_path) = (&pieces, &next, &vol, &volume_path);
                s.spawn(move || -> io::Result<Part> {
                    let h = open(volume_path, GENERIC_READ, 0)?;
                    let mut part = Part::default();
                    let mut buf = vec![0u8; PIECE_BYTES as usize + vol.cluster as usize];
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(p) = pieces.get(i) else { break };
                        if progress.is_cancelled() {
                            break;
                        }
                        // Volume reads must cover whole sectors: round up to clusters.
                        let bytes = (p.count * vol.record).next_multiple_of(vol.cluster as usize);
                        let buf = &mut buf[..bytes];
                        read_at(&h, p.offset, buf)?;
                        let before = part.recs.len();
                        for (k, rec) in buf.chunks_exact_mut(vol.record).take(p.count).enumerate() {
                            parse_record(
                                rec,
                                p.first + k as u64,
                                w as u8,
                                vol.total_bytes,
                                &mut part,
                            );
                        }
                        // A rough live count; replaced by exact totals at the end.
                        progress
                            .files
                            .fetch_add((part.recs.len() - before) as u64, Ordering::Relaxed);
                    }
                    Ok(part)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(bad("MFT reader panicked"))))
            .collect()
    });
    let parts: Vec<Part> = parts.into_iter().collect::<io::Result<_>>()?;
    lap("records read and parsed");
    if progress.is_cancelled() {
        return Ok(());
    }

    // Dense table indexed by record number.
    let total = total as usize;
    let mut recs = vec![Rec::default(); total];
    for part in &parts {
        for &(no, r) in &part.recs {
            if (no as usize) < total {
                recs[no as usize] = r;
            }
        }
    }
    for part in &parts {
        for o in &part.owed {
            if let Some(r) = recs.get_mut(o.base as usize) {
                r.alloc += o.alloc;
                if o.sized && r.flags & (R_SIZED | R_DIR) == 0 {
                    r.size = o.size;
                    r.flags |= R_SIZED;
                }
            }
        }
    }

    // Group children by parent with a counting sort.
    let linked = |i: usize, r: &Rec| {
        r.flags & R_USED != 0 && r.parent as usize != i && (r.parent as usize) < total
    };
    let mut starts = vec![0u32; total + 1];
    for (i, r) in recs.iter().enumerate() {
        if linked(i, r) {
            starts[r.parent as usize + 1] += 1;
        }
    }
    for i in 0..total {
        starts[i + 1] += starts[i];
    }
    let mut cursor = starts.clone();
    let mut order = vec![0u32; starts[total] as usize];
    for (i, r) in recs.iter().enumerate() {
        if linked(i, r) {
            let c = &mut cursor[r.parent as usize];
            order[*c as usize] = i as u32;
            *c += 1;
        }
    }
    drop(cursor);
    lap("children grouped");

    // Breadth-first from the scan root: each directory's children land in
    // one contiguous run, exactly as the tree expects.
    let mut tree = Tree::new(shared.tree.read().root_path.clone());
    tree.nodes[0].mtime = recs[root_rec as usize].mtime;
    let mut queue: VecDeque<(u32, NodeId)> = VecDeque::new();
    queue.push_back((root_rec as u32, 0));
    let mut entries: Vec<NewEntry> = Vec::new();
    let mut names: Vec<u8> = Vec::new();
    let (mut nfiles, mut ndirs) = (0u64, 0u64);
    while let Some((rec, node)) = queue.pop_front() {
        let kids = &order[starts[rec as usize] as usize..starts[rec as usize + 1] as usize];
        if kids.is_empty() {
            continue;
        }
        entries.clear();
        names.clear();
        for &k in kids {
            let r = &recs[k as usize];
            let src = &parts[r.worker as usize].names;
            let name = &src[r.name_off as usize..r.name_off as usize + r.name_len as usize];
            let is_dir = r.flags & R_DIR != 0;
            let mut f = 0u8;
            if is_dir {
                f |= flags::DIR;
                ndirs += 1;
            } else {
                nfiles += 1;
            }
            if r.flags & R_LINK != 0 {
                f |= flags::LINK;
            }
            entries.push(NewEntry {
                name_off: names.len() as u32,
                name_len: r.name_len,
                flags: f,
                kind: if is_dir { 0 } else { kind::categorize(name) },
                size: r.size,
                alloc: r.alloc,
                mtime: r.mtime,
            });
            names.extend_from_slice(name);
        }
        let Some(start) = tree.append_children(node, &entries, &names) else {
            return Err(bad("tree is full"));
        };
        for (i, &k) in kids.iter().enumerate() {
            if recs[k as usize].flags & R_DIR != 0 {
                queue.push_back((k, start + i as u32));
            }
        }
        if progress.is_cancelled() {
            return Ok(());
        }
    }
    lap("tree built");
    tree.shrink();

    let root_alloc = tree.nodes[0].alloc;
    *shared.tree.write() = tree;
    progress.files.store(nfiles, Ordering::Relaxed);
    progress.dirs.store(ndirs, Ordering::Relaxed);
    progress.bytes.store(root_alloc, Ordering::Relaxed);
    Ok(())
}
