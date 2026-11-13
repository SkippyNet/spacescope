//! Live updates: watch the scanned folder and keep the tree current.
//!
//! The watcher only records *which directories* changed. A moment later each
//! of them is re-read once and reconciled with the tree (`refresh_dir`), so
//! a burst of a thousand events in one folder costs a single directory read.
//!
//! * Windows: one recursive `ReadDirectoryChangesW` watch on the root.
//! * Linux: inotify has no recursive mode, so every scanned directory gets
//!   its own watch, taken from the tree we already have (no second walk, and
//!   other filesystems stay excluded). Very large trees are not watched.

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use spacescope_core::{refresh_dir, NodeId, ScanOptions, Shared};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Wait for a quiet moment before applying, so bursts are coalesced.
const SETTLE: Duration = Duration::from_millis(900);
/// Beyond this many pending folders, stop tracking and ask for a rescan.
const MAX_DIRTY: usize = 20_000;

struct Inner {
    dirty: Mutex<HashSet<PathBuf>>,
    last_event: Mutex<Instant>,
    /// Events were dropped: the picture may be stale until a rescan.
    missed: AtomicBool,
    busy: AtomicBool,
    /// Nodes below this index already have their inotify watch (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    watched_upto: AtomicUsize,
    applied: AtomicUsize,
}

pub struct LiveWatch {
    inner: Arc<Inner>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    watcher: Arc<Mutex<RecommendedWatcher>>,
    last_apply: Instant,
}

/// Why live updates are not running.
pub enum Unavailable {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    TooManyFolders(usize),
    Error(String),
}

impl Unavailable {
    pub fn message(&self) -> String {
        match self {
            Unavailable::TooManyFolders(n) => format!(
                "Live updates are off: {n} folders is more than the system can watch at once. \
                 Press F5 to refresh."
            ),
            Unavailable::Error(e) => format!("Live updates are off: {e}. Press F5 to refresh."),
        }
    }
}

#[cfg(target_os = "linux")]
fn watch_limit() -> usize {
    let system = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(8192);
    // Leave room for other programs, and keep kernel memory modest.
    (system * 3 / 4).min(250_000)
}

/// Adds inotify watches for every directory node not yet covered.
#[cfg(target_os = "linux")]
fn sync_watches(shared: &Shared, inner: &Inner, watcher: &Mutex<RecommendedWatcher>) {
    use spacescope_core::flags;
    loop {
        // Collect a bounded batch under the lock, watch outside it.
        let (paths, upto, len) = {
            let tree = shared.tree.read();
            let from = inner.watched_upto.load(Ordering::Acquire);
            let len = tree.len();
            let mut paths = Vec::new();
            let mut i = from;
            while i < len && paths.len() < 4096 {
                let n = &tree.nodes[i];
                let skip = flags::LINK | flags::MOUNT | flags::DEAD | flags::ERROR;
                if n.is_dir() && n.flags & skip == 0 && !tree.is_detached(i as NodeId) {
                    paths.push(tree.path(i as NodeId));
                }
                i += 1;
            }
            (paths, i, len)
        };
        if let Ok(mut w) = watcher.lock() {
            for p in &paths {
                // A folder may have vanished since the scan; that is fine.
                let _ = w.watch(p, RecursiveMode::NonRecursive);
            }
        }
        inner.watched_upto.store(upto, Ordering::Release);
        if upto >= len {
            return;
        }
    }
}

impl LiveWatch {
    /// Starts watching a finished scan. `dirs` is the number of folders in it.
    pub fn start(
        shared: &Arc<Shared>,
        root: &Path,
        dirs: usize,
        ctx: &egui::Context,
    ) -> Result<LiveWatch, Unavailable> {
        #[cfg(target_os = "linux")]
        if dirs > watch_limit() {
            return Err(Unavailable::TooManyFolders(dirs));
        }
        let _ = dirs;

        let inner = Arc::new(Inner {
            dirty: Mutex::new(HashSet::new()),
            last_event: Mutex::new(Instant::now()),
            missed: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            watched_upto: AtomicUsize::new(0),
            applied: AtomicUsize::new(0),
        });
        let (sink, repaint) = (inner.clone(), ctx.clone());
        let handler = move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else {
                sink.missed.store(true, Ordering::Release);
                return;
            };
            if event.need_rescan() {
                sink.missed.store(true, Ordering::Release);
            }
            // Reads do not change sizes.
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            let Ok(mut dirty) = sink.dirty.lock() else {
                return;
            };
            let was_empty = dirty.is_empty();
            for p in &event.paths {
                // A change to an entry is a change to the folder listing it.
                if let Some(parent) = p.parent() {
                    if !dirty.contains(parent) {
                        dirty.insert(parent.to_path_buf());
                    }
                }
            }
            if dirty.len() > MAX_DIRTY {
                dirty.clear();
                sink.missed.store(true, Ordering::Release);
            }
            drop(dirty);
            if let Ok(mut t) = sink.last_event.lock() {
                *t = Instant::now();
            }
            if was_empty {
                repaint.request_repaint_after(SETTLE);
            }
        };
        let mut watcher =
            notify::recommended_watcher(handler).map_err(|e| Unavailable::Error(e.to_string()))?;

        #[cfg(not(target_os = "linux"))]
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| Unavailable::Error(e.to_string()))?;
        #[cfg(target_os = "linux")]
        let _ = (&mut watcher, root);

        let watcher = Arc::new(Mutex::new(watcher));
        #[cfg(target_os = "linux")]
        {
            let (shared, inner, watcher) = (shared.clone(), inner.clone(), watcher.clone());
            std::thread::spawn(move || sync_watches(&shared, &inner, &watcher));
        }
        let _ = shared;
        Ok(LiveWatch {
            inner,
            watcher,
            last_apply: Instant::now(),
        })
    }

    /// Some changes could not be tracked; totals may be stale until a rescan.
    pub fn missed(&self) -> bool {
        self.inner.missed.load(Ordering::Acquire)
    }

    /// A rescan made the picture exact again.
    pub fn clear_missed(&self) {
        self.inner.missed.store(false, Ordering::Release);
    }

    /// Number of folder refreshes applied so far (for tests and the tooltip).
    pub fn applied(&self) -> usize {
        self.inner.applied.load(Ordering::Acquire)
    }

    /// True while there is nothing pending and nothing being applied.
    pub fn idle(&self) -> bool {
        !self.inner.busy.load(Ordering::Acquire)
            && self.inner.dirty.lock().map_or(true, |d| d.is_empty())
    }

    /// Call every frame. Applies pending changes once things have settled.
    pub fn tick(&mut self, shared: &Arc<Shared>, scanning: bool, ctx: &egui::Context) {
        let inner = &self.inner;
        if inner.busy.load(Ordering::Acquire) {
            return;
        }
        let pending = inner.dirty.lock().map_or(0, |d| d.len());
        if pending == 0 {
            return;
        }
        let quiet = inner.last_event.lock().map_or(SETTLE, |t| t.elapsed());
        // Apply after a quiet moment, or regularly under constant activity.
        let due = quiet >= SETTLE || self.last_apply.elapsed() >= 3 * SETTLE;
        if scanning || !due {
            ctx.request_repaint_after(Duration::from_millis(300));
            return;
        }
        let paths: Vec<PathBuf> = match inner.dirty.lock() {
            Ok(mut d) => d.drain().collect(),
            Err(_) => return,
        };
        self.last_apply = Instant::now();
        inner.busy.store(true, Ordering::Release);
        let (shared, inner, ctx) = (shared.clone(), inner.clone(), ctx.clone());
        #[cfg(target_os = "linux")]
        let watcher = self.watcher.clone();
        std::thread::spawn(move || {
            // Several paths can resolve to the same folder (for instance
            // everything inside a folder the tree has not seen yet).
            let nodes: HashSet<NodeId> = {
                let tree = shared.tree.read();
                paths
                    .iter()
                    .filter_map(|p| tree.find_deepest(p))
                    .map(|(n, _)| {
                        if tree.node(n).is_dir() {
                            n
                        } else {
                            tree.node(n).parent
                        }
                    })
                    .filter(|&n| (n as usize) < tree.len())
                    .collect()
            };
            let opts = ScanOptions {
                threads: 2,
                ..Default::default()
            };
            let mut scans = Vec::new();
            for n in nodes {
                let r = refresh_dir(&shared, n, &opts);
                scans.extend(r.scan);
                inner.applied.fetch_add(1, Ordering::AcqRel);
            }
            for s in scans {
                s.wait();
            }
            #[cfg(target_os = "linux")]
            sync_watches(&shared, &inner, &watcher);
            inner.busy.store(false, Ordering::Release);
            ctx.request_repaint();
        });
    }
}
