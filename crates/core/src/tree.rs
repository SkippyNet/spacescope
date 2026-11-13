//! Arena-backed size tree.
//!
//! Every directory's children are appended in one contiguous run, so a
//! directory only stores `(child_start, child_count)`. A child always has a
//! higher index than its parent, which lets whole-tree passes run as a single
//! reverse loop with no recursion.

use parking_lot::RwLock;
use std::borrow::Cow;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub type NodeId = u32;
pub const NO_NODE: NodeId = u32::MAX;

pub mod flags {
    /// Directory.
    pub const DIR: u8 = 1;
    /// Symlink / junction / reparse point. Never followed.
    pub const LINK: u8 = 1 << 1;
    /// The directory could not be read (permissions, vanished, ...).
    pub const ERROR: u8 = 1 << 2;
    /// Removed or replaced by a rescan. Must be skipped everywhere.
    pub const DEAD: u8 = 1 << 3;
    /// Directory on a different filesystem; not descended.
    pub const MOUNT: u8 = 1 << 4;
    /// Additional hard link to a file already counted elsewhere.
    pub const HARDLINK: u8 = 1 << 5;
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Node {
    /// Logical size in bytes. Aggregated for directories.
    pub size: u64,
    /// Bytes allocated on disk. Aggregated for directories.
    pub alloc: u64,
    /// Modification time, unix seconds.
    pub mtime: i64,
    pub parent: NodeId,
    pub child_start: u32,
    pub child_count: u32,
    pub name_off: u32,
    /// Number of files in this subtree (1 for a file).
    pub files: u32,
    pub name_len: u16,
    pub flags: u8,
    /// File category, see [`crate::kind`].
    pub kind: u8,
}

impl Node {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.flags & flags::DIR != 0
    }
    #[inline]
    pub fn is_dead(&self) -> bool {
        self.flags & flags::DEAD != 0
    }
}

/// One directory entry as produced by a platform scanner.
#[derive(Clone, Copy, Debug)]
pub struct NewEntry {
    pub name_off: u32,
    pub name_len: u16,
    pub flags: u8,
    pub kind: u8,
    pub size: u64,
    pub alloc: u64,
    pub mtime: i64,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    names: Vec<u8>,
    /// Additional child runs for directories that gained entries after their
    /// first scan. Empty for a freshly scanned tree.
    extra: std::collections::HashMap<NodeId, Vec<(u32, u32)>>,
    pub root_path: PathBuf,
}

impl Tree {
    pub fn new(root_path: PathBuf) -> Self {
        let mut names = Vec::with_capacity(1 << 16);
        let bytes = root_path.as_os_str().as_encoded_bytes();
        let len = bytes.len().min(u16::MAX as usize);
        names.extend_from_slice(&bytes[..len]);
        let root = Node {
            size: 0,
            alloc: 0,
            mtime: 0,
            parent: NO_NODE,
            child_start: 0,
            child_count: 0,
            name_off: 0,
            files: 0,
            name_len: len as u16,
            flags: flags::DIR,
            kind: 0,
        };
        let mut nodes = Vec::with_capacity(1 << 12);
        nodes.push(root);
        Tree {
            nodes,
            names,
            extra: Default::default(),
            root_path,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Raw name bytes (platform encoding: bytes on unix, WTF-8 on Windows).
    #[inline]
    pub fn name(&self, id: NodeId) -> &[u8] {
        let n = &self.nodes[id as usize];
        let s = n.name_off as usize;
        &self.names[s..s + n.name_len as usize]
    }

    pub fn name_lossy(&self, id: NodeId) -> Cow<'_, str> {
        String::from_utf8_lossy(self.name(id))
    }

    /// Children of a directory: its original contiguous run, then any runs
    /// added later by live updates. May include dead nodes.
    #[inline]
    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let n = &self.nodes[id as usize];
        let main = n.child_start..n.child_start + n.child_count;
        let extra = if self.extra.is_empty() {
            None
        } else {
            self.extra.get(&id)
        };
        main.chain(
            extra
                .into_iter()
                .flatten()
                .flat_map(|&(start, count)| start..start + count),
        )
    }

    /// Finds the live child of `dir` called `name` (case-insensitive on
    /// Windows, like the filesystem).
    pub fn child_named(&self, dir: NodeId, name: &[u8]) -> Option<NodeId> {
        self.children(dir).find(|&c| {
            !self.nodes[c as usize].is_dead() && {
                let have = self.name(c);
                if cfg!(windows) {
                    have.eq_ignore_ascii_case(name)
                } else {
                    have == name
                }
            }
        })
    }

    /// Resolves a filesystem path to the deepest node that exists for it.
    /// Returns `(node, exact)`; `None` if the path is outside this tree.
    pub fn find_deepest(&self, path: &std::path::Path) -> Option<(NodeId, bool)> {
        let rel = path.strip_prefix(&self.root_path).ok()?;
        let mut cur = 0;
        for part in rel.components() {
            let std::path::Component::Normal(name) = part else {
                continue;
            };
            if !self.nodes[cur as usize].is_dir() {
                return Some((self.nodes[cur as usize].parent, false));
            }
            match self.child_named(cur, name.as_encoded_bytes()) {
                Some(c) => cur = c,
                None => return Some((cur, false)),
            }
        }
        Some((cur, true))
    }

    /// Updates a file's sizes in place and adjusts every ancestor.
    pub fn update_file(&mut self, id: NodeId, size: u64, alloc: u64, mtime: i64) {
        let n = &mut self.nodes[id as usize];
        if n.is_dead() || n.is_dir() {
            return;
        }
        let (old_size, old_alloc) = (n.size, n.alloc);
        n.size = size;
        n.alloc = alloc;
        n.mtime = mtime;
        let mut cur = n.parent;
        while cur != NO_NODE {
            let p = &mut self.nodes[cur as usize];
            p.size = p.size.wrapping_add(size).saturating_sub(old_size);
            p.alloc = p.alloc.wrapping_add(alloc).saturating_sub(old_alloc);
            cur = p.parent;
        }
    }

    /// Ancestors from the root (inclusive) down to `id` (inclusive).
    pub fn chain(&self, id: NodeId) -> Vec<NodeId> {
        let mut v = Vec::new();
        let mut cur = id;
        while cur != NO_NODE && (cur as usize) < self.nodes.len() {
            v.push(cur);
            cur = self.nodes[cur as usize].parent;
        }
        v.reverse();
        v
    }

    /// True if `anc` is `id` or one of its ancestors.
    pub fn is_ancestor(&self, anc: NodeId, id: NodeId) -> bool {
        let mut cur = id;
        while cur != NO_NODE {
            if cur == anc {
                return true;
            }
            cur = self.nodes[cur as usize].parent;
        }
        false
    }

    /// True if the node or any of its ancestors was removed.
    pub fn is_detached(&self, id: NodeId) -> bool {
        let mut cur = id;
        while cur != NO_NODE {
            let n = &self.nodes[cur as usize];
            if n.is_dead() {
                return true;
            }
            cur = n.parent;
        }
        false
    }

    /// Full filesystem path of a node.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let chain = self.chain(id);
        let mut p = self.root_path.clone();
        for &c in chain.iter().skip(1) {
            p.push(bytes_to_os(self.name(c)));
        }
        p
    }

    /// Appends the entries of one directory. Returns the id of the first new
    /// node, or `None` if the tree is full.
    pub fn append_children(
        &mut self,
        parent: NodeId,
        entries: &[NewEntry],
        names: &[u8],
    ) -> Option<NodeId> {
        if entries.is_empty() {
            return Some(self.nodes.len() as NodeId);
        }
        let base = self.names.len();
        if base + names.len() >= u32::MAX as usize
            || self.nodes.len() + entries.len() >= (u32::MAX - 16) as usize
        {
            return None;
        }
        // The parent may have been removed while its scan task was queued.
        if self.nodes[parent as usize].is_dead() {
            return None;
        }
        let start = self.nodes.len() as NodeId;
        self.names.extend_from_slice(names);
        self.nodes.reserve(entries.len());
        let (mut dsize, mut dalloc, mut dfiles) = (0u64, 0u64, 0u32);
        for e in entries {
            let is_dir = e.flags & flags::DIR != 0;
            dsize += e.size;
            dalloc += e.alloc;
            dfiles += (!is_dir) as u32;
            self.nodes.push(Node {
                size: e.size,
                alloc: e.alloc,
                mtime: e.mtime,
                parent,
                child_start: 0,
                child_count: 0,
                name_off: base as u32 + e.name_off,
                files: (!is_dir) as u32,
                name_len: e.name_len,
                flags: e.flags,
                kind: e.kind,
            });
        }
        let p = &mut self.nodes[parent as usize];
        if p.child_count == 0 {
            p.child_start = start;
            p.child_count = entries.len() as u32;
        } else {
            // The directory already has its run: a live update found new
            // entries. Existing nodes keep their ids, so views stay valid.
            self.extra
                .entry(parent)
                .or_default()
                .push((start, entries.len() as u32));
        }
        let mut cur = parent;
        while cur != NO_NODE {
            let n = &mut self.nodes[cur as usize];
            n.size = n.size.wrapping_add(dsize);
            n.alloc = n.alloc.wrapping_add(dalloc);
            n.files = n.files.wrapping_add(dfiles);
            cur = n.parent;
        }
        Some(start)
    }

    pub fn mark_error(&mut self, id: NodeId) {
        self.nodes[id as usize].flags |= flags::ERROR;
    }

    fn subtract_from_ancestors(&mut self, id: NodeId, size: u64, alloc: u64, files: u32) {
        let mut cur = self.nodes[id as usize].parent;
        while cur != NO_NODE {
            let n = &mut self.nodes[cur as usize];
            n.size = n.size.saturating_sub(size);
            n.alloc = n.alloc.saturating_sub(alloc);
            n.files = n.files.saturating_sub(files);
            cur = n.parent;
        }
    }

    /// Removes a node (and implicitly its subtree) from all totals.
    pub fn remove(&mut self, id: NodeId) {
        let n = self.nodes[id as usize];
        if n.is_dead() || n.parent == NO_NODE {
            return;
        }
        self.subtract_from_ancestors(id, n.size, n.alloc, n.files);
        let n = &mut self.nodes[id as usize];
        n.flags |= flags::DEAD;
        n.size = 0;
        n.alloc = 0;
        n.files = 0;
    }

    /// Empties a directory ahead of a rescan of that subtree.
    pub fn reset_children(&mut self, id: NodeId) {
        let n = self.nodes[id as usize];
        let kids: Vec<NodeId> = self.children(id).collect();
        for c in kids {
            self.nodes[c as usize].flags |= flags::DEAD;
        }
        self.extra.remove(&id);
        self.subtract_from_ancestors(id, n.size, n.alloc, n.files);
        let n = &mut self.nodes[id as usize];
        n.child_count = 0;
        n.size = 0;
        n.alloc = 0;
        n.files = 0;
        n.flags &= !flags::ERROR;
    }

    /// Releases spare capacity once a scan has finished.
    pub fn shrink(&mut self) {
        // Skip the copy when little would be gained (small live updates).
        if self.nodes.capacity() > self.nodes.len() + self.nodes.len() / 8 + 1024 {
            self.nodes.shrink_to_fit();
        }
        if self.names.capacity() > self.names.len() + self.names.len() / 8 + 65536 {
            self.names.shrink_to_fit();
        }
    }

    /// Approximate heap usage in bytes.
    pub fn memory_bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>() + self.names.capacity()
    }
}

/// Converts stored name bytes back to an `OsString` without loss.
pub fn bytes_to_os(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(bytes).to_os_string()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        match std::str::from_utf8(bytes) {
            Ok(s) => OsString::from(s),
            Err(_) => OsString::from_wide(&wtf8_to_wide(bytes)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Encodes UTF-16 (possibly with unpaired surrogates) as WTF-8.
pub fn push_wtf8(dst: &mut Vec<u8>, wide: &[u16]) {
    if wide.iter().all(|&c| c < 0x80) {
        dst.extend(wide.iter().map(|&c| c as u8));
        return;
    }
    for r in char::decode_utf16(wide.iter().copied()) {
        match r {
            Ok(c) => {
                let mut b = [0u8; 4];
                dst.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            Err(e) => {
                let s = e.unpaired_surrogate() as u32;
                dst.push(0xE0 | (s >> 12) as u8);
                dst.push(0x80 | ((s >> 6) & 0x3F) as u8);
                dst.push(0x80 | (s & 0x3F) as u8);
            }
        }
    }
}

/// Decodes WTF-8 back to UTF-16.
pub fn wtf8_to_wide(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i] as u32;
        let (cp, n) = if b < 0x80 {
            (b, 1)
        } else if b >> 5 == 0b110 && i + 1 < bytes.len() {
            (((b & 0x1F) << 6) | (bytes[i + 1] as u32 & 0x3F), 2)
        } else if b >> 4 == 0b1110 && i + 2 < bytes.len() {
            (
                ((b & 0x0F) << 12)
                    | ((bytes[i + 1] as u32 & 0x3F) << 6)
                    | (bytes[i + 2] as u32 & 0x3F),
                3,
            )
        } else if b >> 3 == 0b11110 && i + 3 < bytes.len() {
            (
                ((b & 0x07) << 18)
                    | ((bytes[i + 1] as u32 & 0x3F) << 12)
                    | ((bytes[i + 2] as u32 & 0x3F) << 6)
                    | (bytes[i + 3] as u32 & 0x3F),
                4,
            )
        } else {
            (0xFFFD, 1)
        };
        if cp >= 0x10000 {
            let c = cp - 0x10000;
            out.push(0xD800 | (c >> 10) as u16);
            out.push(0xDC00 | (c & 0x3FF) as u16);
        } else {
            out.push(cp as u16);
        }
        i += n;
    }
    out
}

/// A tree shared between scanner threads, the layout thread and the UI.
pub struct Shared {
    pub tree: RwLock<Tree>,
    /// Bumped on every mutation so readers can tell when to refresh.
    pub generation: AtomicU64,
}

impl Shared {
    pub fn new(tree: Tree) -> Self {
        Shared {
            tree: RwLock::new(tree),
            generation: AtomicU64::new(1),
        }
    }
    #[inline]
    pub fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }
    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(off: u32, len: u16, dir: bool, size: u64) -> NewEntry {
        NewEntry {
            name_off: off,
            name_len: len,
            flags: if dir { flags::DIR } else { 0 },
            kind: 0,
            size,
            alloc: size,
            mtime: 0,
        }
    }

    #[test]
    fn aggregate_and_remove() {
        let mut t = Tree::new(PathBuf::from("root"));
        let a = t
            .append_children(0, &[entry(0, 1, true, 0), entry(1, 1, false, 10)], b"ab")
            .unwrap();
        let c = t
            .append_children(a, &[entry(0, 1, false, 5), entry(1, 1, false, 7)], b"cd")
            .unwrap();
        assert_eq!(t.node(0).size, 22);
        assert_eq!(t.node(0).files, 3);
        assert_eq!(t.node(a).size, 12);
        assert_eq!(t.name(c + 1), b"d");
        assert_eq!(t.path(c), PathBuf::from("root").join("a").join("c"));

        t.remove(c);
        assert_eq!(t.node(0).size, 17);
        assert_eq!(t.node(a).files, 1);

        t.reset_children(a);
        assert_eq!(t.node(0).size, 10);
        assert_eq!(t.node(0).files, 1);
        assert!(t.is_detached(c + 1));
    }

    #[test]
    fn wtf8_roundtrip() {
        let wide: Vec<u16> = vec![0x41, 0xD800, 0x42, 0xD83D, 0xDE00, 0x4E2D];
        let mut b = Vec::new();
        push_wtf8(&mut b, &wide);
        assert_eq!(wtf8_to_wide(&b), wide);
    }
}
