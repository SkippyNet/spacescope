//! Parallel directory scanner.
//!
//! A pool of workers shares a work-stealing queue of directories. Each worker
//! reads one directory with the cheapest OS call available, appends the whole
//! batch to the shared tree under a short write lock, then queues the
//! sub-directories it found.

use crate::kind;
use crate::tree::{bytes_to_os, flags, NewEntry, NodeId, Shared, Tree};
use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod generic;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod mft;
#[cfg(windows)]
mod windows;

#[derive(Clone, Default)]
pub struct ScanOptions {
    /// Worker threads. 0 picks a default from the CPU count.
    pub threads: usize,
    /// Called once when the scan finishes or is cancelled.
    pub on_done: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Never read the NTFS master file table directly, even when allowed to.
    pub no_mft: bool,
}

pub fn default_threads() -> usize {
    // Directory enumeration is bound by kernel locks rather than CPU: past
    // about one thread per physical core it measurably slows down.
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    (n / 2).clamp(4, 16)
}

/// Live counters for a running scan.
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    /// Bytes allocated on disk seen so far.
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    /// Directories on another filesystem that were not descended.
    pub mounts: AtomicU64,
    mft: AtomicBool,
    cancel: AtomicBool,
    done: AtomicBool,
    started: Instant,
    finished_micros: AtomicU64,
}

impl Progress {
    fn new() -> Self {
        Progress {
            files: AtomicU64::new(0),
            dirs: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            mounts: AtomicU64::new(0),
            mft: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            done: AtomicBool::new(false),
            started: Instant::now(),
            finished_micros: AtomicU64::new(0),
        }
    }
    /// True if the scan read the NTFS master file table instead of walking.
    pub fn used_mft(&self) -> bool {
        self.mft.load(Ordering::Acquire)
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
    pub fn elapsed(&self) -> Duration {
        if self.is_done() {
            Duration::from_micros(self.finished_micros.load(Ordering::Acquire))
        } else {
            self.started.elapsed()
        }
    }
    /// Blocks until the scan has finished.
    pub fn wait(&self) {
        while !self.is_done() {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Per-thread reusable buffers.
#[derive(Default)]
pub(crate) struct Scratch {
    pub entries: Vec<NewEntry>,
    pub names: Vec<u8>,
    /// 8-byte aligned raw buffer for directory enumeration syscalls.
    #[allow(dead_code)]
    pub buf: Vec<u64>,
    #[allow(dead_code)]
    pub wide: Vec<u16>,
}

impl Scratch {
    #[inline]
    pub fn push(&mut self, name: &[u8], flags: u8, size: u64, alloc: u64, mtime: i64) {
        let len = name.len().min(u16::MAX as usize);
        let off = self.names.len() as u32;
        self.names.extend_from_slice(&name[..len]);
        self.entries.push(NewEntry {
            name_off: off,
            name_len: len as u16,
            flags,
            kind: 0,
            size,
            alloc,
            mtime,
        });
    }
}

/// State shared by the platform readers.
pub(crate) struct Platform {
    /// Device id of the scan root; other devices are not descended (unix).
    #[allow(dead_code)]
    pub dev: u64,
    /// `(dev, inode)` of multiply-linked files already counted (unix).
    #[allow(dead_code)]
    pub hardlinks: Mutex<HashSet<(u64, u64)>>,
}

struct Task {
    node: NodeId,
    path: PathBuf,
}

struct Ctx {
    shared: Arc<Shared>,
    progress: Arc<Progress>,
    platform: Platform,
    injector: Injector<Task>,
    stealers: Vec<Stealer<Task>>,
    /// Queued + in-flight tasks.
    pending: AtomicUsize,
    alive: AtomicUsize,
    on_done: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn read_dir(path: &Path, s: &mut Scratch, p: &Platform) -> io::Result<()> {
    s.entries.clear();
    s.names.clear();
    #[cfg(windows)]
    {
        match windows::read_dir(path, s, p) {
            Err(e) if windows::should_fall_back(&e) => {
                s.entries.clear();
                s.names.clear();
                generic::read_dir(path, s, p)
            }
            r => r,
        }
    }
    #[cfg(target_os = "linux")]
    {
        linux::read_dir(path, s, p)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        generic::read_dir(path, s, p)
    }
}

fn process(task: Task, local: &Worker<Task>, ctx: &Ctx, s: &mut Scratch) {
    let result = read_dir(&task.path, s, &ctx.platform);
    if result.is_err() {
        ctx.progress.errors.fetch_add(1, Ordering::Relaxed);
    }
    // Keep whatever was read before an error occurred.
    let (mut nfiles, mut ndirs, mut bytes) = (0u64, 0u64, 0u64);
    for e in s.entries.iter_mut() {
        bytes += e.alloc;
        if e.flags & flags::DIR != 0 {
            ndirs += 1;
            if e.flags & flags::MOUNT != 0 {
                ctx.progress.mounts.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            nfiles += 1;
            let o = e.name_off as usize;
            let by_name = kind::categorize(&s.names[o..o + e.name_len as usize]);
            // The reader may pre-set a hint (e.g. executable bit on unix).
            if by_name != kind::OTHER || e.kind == kind::OTHER {
                e.kind = by_name;
            }
        }
    }
    let start = {
        let mut tree = ctx.shared.tree.write();
        if result.is_err() {
            tree.mark_error(task.node);
        }
        tree.append_children(task.node, &s.entries, &s.names)
    };
    ctx.shared.bump();
    let p = &ctx.progress;
    p.files.fetch_add(nfiles, Ordering::Relaxed);
    p.dirs.fetch_add(ndirs, Ordering::Relaxed);
    p.bytes.fetch_add(bytes, Ordering::Relaxed);

    let Some(start) = start else { return };
    const SKIP: u8 = flags::LINK | flags::MOUNT | flags::ERROR;
    for (i, e) in s.entries.iter().enumerate() {
        if e.flags & flags::DIR != 0 && e.flags & SKIP == 0 {
            let o = e.name_off as usize;
            let name = &s.names[o..o + e.name_len as usize];
            ctx.pending.fetch_add(1, Ordering::AcqRel);
            local.push(Task {
                node: start + i as u32,
                path: task.path.join(bytes_to_os(name)),
            });
        }
    }
}

fn find_task(local: &Worker<Task>, ctx: &Ctx, me: usize) -> Option<Task> {
    if let Some(t) = local.pop() {
        return Some(t);
    }
    loop {
        let mut retry = false;
        match ctx.injector.steal_batch_and_pop(local) {
            Steal::Success(t) => return Some(t),
            Steal::Retry => retry = true,
            Steal::Empty => {}
        }
        let n = ctx.stealers.len();
        for k in 1..n {
            match ctx.stealers[(me + k) % n].steal() {
                Steal::Success(t) => return Some(t),
                Steal::Retry => retry = true,
                Steal::Empty => {}
            }
        }
        if !retry {
            return None;
        }
    }
}

fn worker(local: Worker<Task>, ctx: Arc<Ctx>, me: usize) {
    let mut scratch = Scratch::default();
    let mut idle = 0u32;
    loop {
        if ctx.progress.is_cancelled() {
            break;
        }
        match find_task(&local, &ctx, me) {
            Some(task) => {
                idle = 0;
                // A panic in one directory must never hang the whole scan.
                let run =
                    std::panic::AssertUnwindSafe(|| process(task, &local, &ctx, &mut scratch));
                if std::panic::catch_unwind(run).is_err() {
                    ctx.progress.errors.fetch_add(1, Ordering::Relaxed);
                }
                ctx.pending.fetch_sub(1, Ordering::AcqRel);
            }
            None => {
                if ctx.pending.load(Ordering::Acquire) == 0 {
                    break;
                }
                idle += 1;
                if idle < 64 {
                    std::thread::yield_now();
                } else {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        }
    }
    worker_exit(&ctx);
}

/// Called once per worker slot; the last one out marks the scan finished.
fn worker_exit(ctx: &Ctx) {
    if ctx.alive.fetch_sub(1, Ordering::AcqRel) == 1 {
        finish(&ctx.shared, &ctx.progress, &ctx.on_done);
    }
}

fn finish(shared: &Shared, p: &Progress, on_done: &Option<Arc<dyn Fn() + Send + Sync>>) {
    // Growth doubling can leave the arena half empty; give it back.
    shared.tree.write().shrink();
    p.finished_micros
        .store(p.started.elapsed().as_micros() as u64, Ordering::Release);
    p.done.store(true, Ordering::Release);
    shared.bump();
    if let Some(cb) = on_done {
        cb();
    }
}

fn device_of(path: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).map(|m| m.dev()).unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0
    }
}

fn spawn(shared: Arc<Shared>, start: Vec<Task>, dev: u64, opts: &ScanOptions) -> Arc<Progress> {
    let progress = Arc::new(Progress::new());
    spawn_with(shared, start, dev, opts, progress.clone());
    progress
}

fn spawn_with(
    shared: Arc<Shared>,
    start: Vec<Task>,
    dev: u64,
    opts: &ScanOptions,
    progress: Arc<Progress>,
) {
    let threads = if opts.threads == 0 {
        default_threads()
    } else {
        opts.threads.min(256)
    };
    let workers: Vec<Worker<Task>> = (0..threads).map(|_| Worker::new_lifo()).collect();
    let ctx = Arc::new(Ctx {
        shared,
        progress: progress.clone(),
        platform: Platform {
            dev,
            hardlinks: Mutex::new(HashSet::new()),
        },
        injector: Injector::new(),
        stealers: workers.iter().map(|w| w.stealer()).collect(),
        pending: AtomicUsize::new(start.len()),
        alive: AtomicUsize::new(threads),
        on_done: opts.on_done.clone(),
    });
    for t in start {
        ctx.injector.push(t);
    }
    for (i, w) in workers.into_iter().enumerate() {
        let c = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("scan-{i}"))
            .spawn(move || worker(w, c, i));
        if spawned.is_err() {
            // Could not start this worker: release its slot so the scan can
            // still finish (its queue is stolen by the others).
            worker_exit(&ctx);
        }
    }
}

/// Strips the `\\?\` prefix Windows adds when canonicalising.
pub fn display_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.as_os_str().to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest.to_string());
        }
    }
    p.to_path_buf()
}

/// Starts scanning `root`. Returns immediately; the tree fills in live.
pub fn scan_path(root: &Path, opts: &ScanOptions) -> io::Result<(Arc<Shared>, Arc<Progress>)> {
    let canon = std::fs::canonicalize(root)?;
    if !std::fs::metadata(&canon)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    let shown = display_path(&canon);
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut tree = Tree::new(shown.clone());
    // Count the root directory itself, as `du` does.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(md) = std::fs::metadata(&canon) {
            tree.nodes[0].alloc = md.blocks() * 512;
            tree.nodes[0].mtime = md.mtime();
        }
    }
    let shared = Arc::new(Shared::new(tree));
    let dev = device_of(&canon);
    let progress = Arc::new(Progress::new());
    let start = vec![Task {
        node: 0,
        path: shown.clone(),
    }];

    // On NTFS with enough rights, read the master file table instead of
    // walking. Anything unexpected falls back to the walk, same progress.
    #[cfg(windows)]
    // Reading the whole table costs a couple of seconds whatever the folder,
    // so it only pays off for drives and their top-level folders.
    if !opts.no_mft && mft::drive_of(&shown).is_some() && shown.components().count() <= 4 {
        let (shared2, progress2, opts2) = (shared.clone(), progress.clone(), opts.clone());
        let spawned =
            std::thread::Builder::new()
                .name("mft".into())
                .spawn(move || match mft::scan(&shared2, &shown, &progress2) {
                    Ok(()) => {
                        progress2.mft.store(true, Ordering::Release);
                        finish(&shared2, &progress2, &opts2.on_done);
                    }
                    Err(_) => {
                        progress2.files.store(0, Ordering::Relaxed);
                        spawn_with(shared2, start, dev, &opts2, progress2);
                    }
                });
        if spawned.is_ok() {
            return Ok((shared, progress));
        }
        // The thread (and with it `start`) is gone: nothing sane to do but report it.
        return Err(io::Error::other("could not start the scan thread"));
    }

    spawn_with(shared.clone(), start, dev, opts, progress.clone());
    Ok((shared, progress))
}

/// Re-reads one directory subtree in place.
pub fn rescan_subtree(shared: &Arc<Shared>, node: NodeId, opts: &ScanOptions) -> Arc<Progress> {
    let path = {
        let mut tree = shared.tree.write();
        let path = tree.path(node);
        tree.reset_children(node);
        path
    };
    shared.bump();
    let dev = device_of(&path);
    spawn(shared.clone(), vec![Task { node, path }], dev, opts)
}

/// What [`refresh_dir`] found.
pub struct Refresh {
    /// Something in the tree changed.
    pub changed: bool,
    /// Running scan of newly appeared sub-directories, if any.
    pub scan: Option<Arc<Progress>>,
}

/// Re-reads a single directory (not its sub-directories) and reconciles the
/// tree with what is on disk now: sizes are updated in place, vanished
/// entries are removed, new ones are added and, if they are directories,
/// scanned. Existing nodes keep their ids. This is what live watching calls
/// for each directory that reported a change.
pub fn refresh_dir(shared: &Arc<Shared>, node: NodeId, opts: &ScanOptions) -> Refresh {
    let unchanged = Refresh {
        changed: false,
        scan: None,
    };
    let path = {
        let tree = shared.tree.read();
        if node as usize >= tree.len() || tree.is_detached(node) {
            return unchanged;
        }
        let n = tree.node(node);
        if !n.is_dir() || n.flags & (flags::LINK | flags::MOUNT) != 0 {
            return unchanged;
        }
        tree.path(node)
    };
    let dev = device_of(&path);
    let platform = Platform {
        dev,
        hardlinks: Mutex::new(HashSet::new()),
    };
    let mut s = Scratch::default();
    if let Err(e) = read_dir(&path, &mut s, &platform) {
        if e.kind() != io::ErrorKind::NotFound {
            return unchanged; // e.g. permissions: leave what we have
        }
        // The directory itself is gone.
        shared.tree.write().remove(node);
        shared.bump();
        return Refresh {
            changed: true,
            scan: None,
        };
    }
    for e in s.entries.iter_mut() {
        if e.flags & flags::DIR == 0 {
            let o = e.name_off as usize;
            let by_name = kind::categorize(&s.names[o..o + e.name_len as usize]);
            if by_name != kind::OTHER || e.kind == kind::OTHER {
                e.kind = by_name;
            }
        }
    }
    let key = |name: &[u8]| -> Vec<u8> {
        if cfg!(windows) {
            name.to_ascii_lowercase()
        } else {
            name.to_vec()
        }
    };

    let mut tree = shared.tree.write();
    let mut existing: std::collections::HashMap<Vec<u8>, NodeId> = tree
        .children(node)
        .filter(|&c| !tree.node(c).is_dead())
        .map(|c| (key(tree.name(c)), c))
        .collect();
    let mut changed = false;
    let mut added: Vec<NewEntry> = Vec::new();
    let mut added_names: Vec<u8> = Vec::new();
    for e in &s.entries {
        let o = e.name_off as usize;
        let name = &s.names[o..o + e.name_len as usize];
        let is_dir = e.flags & flags::DIR != 0;
        if let Some(id) = existing.remove(&key(name)) {
            let old = *tree.node(id);
            if old.is_dir() == is_dir {
                // A hard link counted elsewhere stays weightless.
                let differs = old.size != e.size || old.alloc != e.alloc || old.mtime != e.mtime;
                if !is_dir && old.flags & flags::HARDLINK == 0 && differs {
                    tree.update_file(id, e.size, e.alloc, e.mtime);
                    changed = true;
                }
                continue;
            }
            tree.remove(id); // a file became a folder or the reverse
        }
        added.push(NewEntry {
            name_off: added_names.len() as u32,
            ..*e
        });
        added_names.extend_from_slice(name);
    }
    for (_, id) in existing {
        tree.remove(id);
        changed = true;
    }
    let mut tasks = Vec::new();
    if !added.is_empty() {
        changed = true;
        if let Some(start) = tree.append_children(node, &added, &added_names) {
            const SKIP: u8 = flags::LINK | flags::MOUNT | flags::ERROR;
            for (i, e) in added.iter().enumerate() {
                if e.flags & flags::DIR != 0 && e.flags & SKIP == 0 {
                    let o = e.name_off as usize;
                    let name = &added_names[o..o + e.name_len as usize];
                    tasks.push(Task {
                        node: start + i as u32,
                        path: path.join(bytes_to_os(name)),
                    });
                }
            }
        }
    }
    drop(tree);
    if changed {
        shared.bump();
    }
    let scan = (!tasks.is_empty()).then(|| spawn(shared.clone(), tasks, dev, opts));
    Refresh { changed, scan }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("spacescope-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn scan_matches_reality() {
        let d = temp_dir("scan");
        let mut expect_size = 0u64;
        let mut expect_files = 0u32;
        for a in 0..6 {
            let sub = d
                .join(format!("dir{a}"))
                .join("nested")
                .join("deeper ünïcödé");
            fs::create_dir_all(&sub).unwrap();
            for f in 0..25 {
                let len = (a * 1000 + f * 37) as usize;
                fs::write(sub.join(format!("file{f}.bin")), vec![7u8; len]).unwrap();
                expect_size += len as u64;
                expect_files += 1;
            }
        }
        fs::write(d.join("top.txt"), b"hello").unwrap();
        expect_size += 5;
        expect_files += 1;
        fs::create_dir(d.join("empty")).unwrap();

        let (shared, progress) = scan_path(&d, &ScanOptions::default()).unwrap();
        progress.wait();
        {
            let tree = shared.tree.read();
            assert_eq!(tree.node(0).size, expect_size);
            assert_eq!(tree.node(0).files, expect_files);
            assert_eq!(progress.files.load(Ordering::Relaxed), expect_files as u64);
            assert_eq!(progress.errors.load(Ordering::Relaxed), 0);
            // Every node's path must exist on disk.
            for id in 1..tree.len() as u32 {
                assert!(tree.path(id).exists(), "{:?}", tree.path(id));
            }
        }

        // Delete a subtree and rescan in place.
        fs::remove_dir_all(d.join("dir0")).unwrap();
        fs::write(d.join("new.bin"), vec![1u8; 4096]).unwrap();
        let p2 = rescan_subtree(&shared, 0, &ScanOptions::default());
        p2.wait();
        let tree = shared.tree.read();
        let removed: u64 = (0..25).map(|f| (f * 37) as u64).sum();
        assert_eq!(tree.node(0).size, expect_size - removed + 4096);
        assert_eq!(tree.node(0).files, expect_files - 25 + 1);
        drop(tree);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn refresh_reconciles_one_directory() {
        let d = temp_dir("refresh");
        fs::create_dir_all(d.join("keep/inner")).unwrap();
        fs::write(d.join("keep/inner/deep.bin"), vec![0u8; 500]).unwrap();
        fs::write(d.join("grow.bin"), vec![0u8; 100]).unwrap();
        fs::write(d.join("gone.bin"), vec![0u8; 40]).unwrap();
        let opts = ScanOptions::default();
        let (shared, progress) = scan_path(&d, &opts).unwrap();
        progress.wait();
        // The tree holds the canonical path, which can differ from the one
        // asked for (8.3 short names in %TEMP%, symlinked /tmp).
        let d = shared.tree.read().root_path.clone();
        let keep = shared.tree.read().find_deepest(&d.join("keep")).unwrap();
        assert!(keep.1);

        // Nothing changed: nothing to do.
        let r = refresh_dir(&shared, 0, &opts);
        assert!(!r.changed && r.scan.is_none());

        fs::write(d.join("grow.bin"), vec![0u8; 1000]).unwrap();
        fs::remove_file(d.join("gone.bin")).unwrap();
        fs::write(d.join("new.bin"), vec![0u8; 7]).unwrap();
        fs::create_dir_all(d.join("newdir/sub")).unwrap();
        fs::write(d.join("newdir/sub/x.bin"), vec![0u8; 30]).unwrap();

        let r = refresh_dir(&shared, 0, &opts);
        assert!(r.changed);
        r.scan.expect("new directory must be scanned").wait();
        let tree = shared.tree.read();
        assert_eq!(tree.node(0).size, 500 + 1000 + 7 + 30);
        assert_eq!(tree.node(0).files, 4);
        // Untouched folders keep their node ids, so open views stay valid.
        assert_eq!(tree.find_deepest(&d.join("keep")), Some(keep));
        let x = tree.find_deepest(&d.join("newdir/sub/x.bin")).unwrap();
        assert!(x.1 && tree.node(x.0).size == 30);
        assert!(!tree.find_deepest(&d.join("gone.bin")).unwrap().1);
        drop(tree);

        // A folder that vanished is removed by refreshing it.
        fs::remove_dir_all(d.join("keep")).unwrap();
        assert!(refresh_dir(&shared, keep.0, &opts).changed);
        assert_eq!(shared.tree.read().node(0).size, 1000 + 7 + 30);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_root_is_an_error() {
        assert!(scan_path(
            Path::new("/definitely/not/here/xyz"),
            &ScanOptions::default()
        )
        .is_err());
    }
}
