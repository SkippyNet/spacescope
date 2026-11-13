//! CSV export of a subtree.

use crate::fmt;
use crate::tree::{flags, NodeId, Tree};
use std::io::{self, Write};

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Writes one row per file and directory under `root`, largest first within
/// each directory. Returns the number of rows written.
pub fn write_csv<W: Write>(tree: &Tree, root: NodeId, mut w: W) -> io::Result<u64> {
    writeln!(w, "Path,Type,Size,Allocated,Files,Modified (UTC)")?;
    let mut rows = 0u64;
    let mut stack = vec![root];
    let mut kids: Vec<NodeId> = Vec::new();
    while let Some(id) = stack.pop() {
        let n = tree.node(id);
        if n.is_dead() {
            continue;
        }
        let ty = if n.flags & flags::LINK != 0 {
            "link"
        } else if n.is_dir() {
            "dir"
        } else {
            "file"
        };
        writeln!(
            w,
            "{},{},{},{},{},{}",
            quote(&tree.path(id).to_string_lossy()),
            ty,
            n.size,
            n.alloc,
            n.files,
            fmt::datetime(n.mtime)
        )?;
        rows += 1;
        if n.is_dir() {
            kids.clear();
            kids.extend(tree.children(id));
            // Smallest pushed last-to-first so the largest pops first.
            kids.sort_unstable_by_key(|&c| tree.node(c).alloc);
            stack.extend_from_slice(&kids);
        }
    }
    w.flush()?;
    Ok(rows)
}
