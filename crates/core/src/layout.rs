//! Squarified treemap layout (Bruls, Huizing, van Wijk).
//!
//! Only what can actually be seen is laid out: directories smaller than
//! `min_dir` pixels stay collapsed, and children that would be smaller than
//! `min_tile` pixels are merged into a single "more" tile.

use crate::tree::{flags, NodeId, Tree};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metric {
    /// Bytes allocated on disk.
    Allocated,
    /// Logical file size.
    Logical,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum TileKind {
    File,
    /// Expanded directory: a frame that contains other tiles.
    Dir,
    /// Directory too small to expand.
    DirCollapsed,
    /// Many small siblings merged together. `node` is the parent directory.
    More,
    /// Free space on the volume.
    Free,
}

#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub weight: u64,
    pub node: NodeId,
    /// Files in the subtree, or merged item count for `More`.
    pub count: u32,
    pub depth: u8,
    pub kind: TileKind,
    /// File category, see [`crate::kind`].
    pub cat: u8,
    /// Expanded directory with room for a title strip.
    pub header: bool,
    /// Index of the enclosing directory tile, `u32::MAX` for the root tile.
    pub parent: u32,
}

impl Tile {
    #[inline]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

#[derive(Clone, Debug)]
pub struct LayoutOptions {
    pub width: f32,
    pub height: f32,
    pub metric: Metric,
    /// Smallest tile side worth drawing, in pixels.
    pub min_tile: f32,
    /// Directories narrower or shorter than this stay collapsed.
    pub min_dir: f32,
    pub header_h: f32,
    pub pad: f32,
    pub max_tiles: usize,
    /// Free bytes to show next to the root's children (0 = hidden).
    pub free_space: u64,
    /// Tiles at least this large get their name extracted for labelling.
    pub label_min_w: f32,
    pub label_min_h: f32,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        LayoutOptions {
            width: 1280.0,
            height: 720.0,
            metric: Metric::Allocated,
            min_tile: 4.0,
            min_dir: 26.0,
            header_h: 18.0,
            pad: 2.0,
            max_tiles: 120_000,
            free_space: 0,
            label_min_w: 34.0,
            label_min_h: 15.0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    /// Paint order: a directory always precedes its content.
    pub tiles: Vec<Tile>,
    /// `(tile index, name)` for tiles large enough to label.
    pub labels: Vec<(u32, String)>,
    pub root: NodeId,
    pub root_weight: u64,
    pub width: f32,
    pub height: f32,
}

impl Layout {
    /// Index of the deepest tile under a point.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.tiles.iter().rposition(|t| t.contains(x, y))
    }

    /// Index of the tile showing `node` as itself (not a `More`/`Free` tile).
    pub fn find(&self, node: NodeId) -> Option<usize> {
        self.tiles
            .iter()
            .position(|t| t.node == node && !matches!(t.kind, TileKind::More | TileKind::Free))
    }
}

const FREE: NodeId = u32::MAX;

/// Lays out the subtree under `root`. `weights`, when given, overrides the
/// metric (used for filtered views).
pub fn compute(tree: &Tree, weights: Option<&[u64]>, root: NodeId, o: &LayoutOptions) -> Layout {
    let mut out = Layout {
        tiles: Vec::new(),
        labels: Vec::new(),
        root,
        root_weight: 0,
        width: o.width,
        height: o.height,
    };
    if root as usize >= tree.len() || o.width < 4.0 || o.height < 4.0 || tree.is_detached(root) {
        return out;
    }
    let wt = |id: NodeId| -> u64 {
        match weights {
            Some(w) => w.get(id as usize).copied().unwrap_or(0),
            None => {
                let n = &tree.nodes[id as usize];
                match o.metric {
                    Metric::Allocated => n.alloc,
                    Metric::Logical => n.size,
                }
            }
        }
    };
    out.root_weight = wt(root);
    out.tiles.reserve(4096);

    let min_area = (o.min_tile * o.min_tile) as f64;
    let mut stack: Vec<(NodeId, [f32; 4], u8, u32)> =
        vec![(root, [0.0, 0.0, o.width, o.height], 0, u32::MAX)];
    let mut kids: Vec<(u64, NodeId)> = Vec::new();

    while let Some((id, [x, y, w, h], depth, parent)) = stack.pop() {
        let n = &tree.nodes[id as usize];
        let weight = wt(id);
        let is_dir = n.is_dir();
        let expand = is_dir
            && n.child_count > 0
            && n.flags & (flags::LINK | flags::MOUNT) == 0
            && w >= o.min_dir
            && h >= o.min_dir
            && depth < 250
            && out.tiles.len() < o.max_tiles;
        if !expand {
            out.tiles.push(Tile {
                x,
                y,
                w,
                h,
                weight,
                node: id,
                count: n.files,
                depth,
                kind: if is_dir {
                    TileKind::DirCollapsed
                } else {
                    TileKind::File
                },
                cat: n.kind,
                header: false,
                parent,
            });
            continue;
        }

        let header = h >= o.header_h * 2.2 && w >= 48.0;
        out.tiles.push(Tile {
            x,
            y,
            w,
            h,
            weight,
            node: id,
            count: n.files,
            depth,
            kind: TileKind::Dir,
            cat: 0,
            header,
            parent,
        });
        let me = (out.tiles.len() - 1) as u32;
        let top = if header { o.header_h } else { o.pad };
        let (ix, iy, iw, ih) = (x + o.pad, y + top, w - 2.0 * o.pad, h - top - o.pad);
        if iw < 3.0 || ih < 3.0 {
            continue;
        }

        kids.clear();
        let free = if depth == 0 { o.free_space } else { 0 };
        let mut total: u128 = free as u128;
        for c in tree.children(id) {
            if tree.nodes[c as usize].is_dead() {
                continue;
            }
            let cw = wt(c);
            if cw > 0 {
                total += cw as u128;
                kids.push((cw, c));
            }
        }
        if total == 0 {
            continue;
        }
        let scale = (iw as f64 * ih as f64) / total as f64;

        // Children far below the visible threshold never need sorting.
        let cut = (min_area * 0.5 / scale) as u64;
        let (mut rest_w, mut rest_n) = (0u64, 0u32);
        if cut > 0 {
            kids.retain(|&(cw, _)| {
                if cw < cut {
                    rest_w += cw;
                    rest_n += 1;
                    false
                } else {
                    true
                }
            });
        }
        if free > 0 {
            kids.push((free, FREE));
        }
        kids.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

        // Squarify into the shrinking rectangle (rx, ry, rw, rh).
        let (mut rx, mut ry, mut rw, mut rh) = (ix as f64, iy as f64, iw as f64, ih as f64);
        let mut i = 0usize;
        while i < kids.len() && rw > 0.5 && rh > 0.5 {
            let first = kids[i].0 as f64 * scale;
            if first < min_area {
                break;
            }
            let side = rw.min(rh);
            let s2 = side * side;
            let (mut sum, max) = (first, first);
            let mut worst = (s2 * max / (sum * sum)).max(sum * sum / (s2 * first));
            let mut j = i + 1;
            while j < kids.len() {
                let a = kids[j].0 as f64 * scale;
                let ns = sum + a;
                let nw = (s2 * max / (ns * ns)).max(ns * ns / (s2 * a));
                if nw > worst {
                    break;
                }
                sum = ns;
                worst = nw;
                j += 1;
            }
            let horizontal = rw >= rh; // strip runs down the left edge
            let thick = if horizontal { sum / rh } else { sum / rw };
            let mut pos = if horizontal { ry } else { rx };
            for &(cw, c) in &kids[i..j] {
                let len = cw as f64 * scale / thick;
                let rect = if horizontal {
                    [rx as f32, pos as f32, thick as f32, len as f32]
                } else {
                    [pos as f32, ry as f32, len as f32, thick as f32]
                };
                pos += len;
                if c == FREE {
                    out.tiles.push(Tile {
                        x: rect[0],
                        y: rect[1],
                        w: rect[2],
                        h: rect[3],
                        weight: cw,
                        node: id,
                        count: 0,
                        depth: depth + 1,
                        kind: TileKind::Free,
                        cat: 0,
                        header: false,
                        parent: me,
                    });
                } else {
                    stack.push((c, rect, depth + 1, me));
                }
            }
            if horizontal {
                rx += thick;
                rw -= thick;
            } else {
                ry += thick;
                rh -= thick;
            }
            i = j;
        }
        for &(cw, c) in &kids[i..] {
            if c != FREE {
                rest_w += cw;
                rest_n += 1;
            }
        }
        if rest_n > 0 && rw >= 1.0 && rh >= 1.0 {
            out.tiles.push(Tile {
                x: rx as f32,
                y: ry as f32,
                w: rw as f32,
                h: rh as f32,
                weight: rest_w,
                node: id,
                count: rest_n,
                depth: depth + 1,
                kind: TileKind::More,
                cat: 0,
                header: false,
                parent: me,
            });
        }
    }

    for (i, t) in out.tiles.iter().enumerate() {
        let big = t.w >= o.label_min_w && t.h >= o.label_min_h;
        let text = match t.kind {
            TileKind::Dir if t.header => tree.name_lossy(t.node).into_owned(),
            TileKind::File | TileKind::DirCollapsed if big => tree.name_lossy(t.node).into_owned(),
            TileKind::More if big => {
                format!("{} smaller items", crate::fmt::human_count(t.count as u64))
            }
            TileKind::Free if big => "Free space".to_string(),
            _ => continue,
        };
        out.labels.push((i as u32, text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NewEntry;
    use std::path::PathBuf;

    fn e(dir: bool, size: u64) -> NewEntry {
        NewEntry {
            name_off: 0,
            name_len: 1,
            flags: if dir { flags::DIR } else { 0 },
            kind: 0,
            size,
            alloc: size,
            mtime: 0,
        }
    }

    fn sample() -> Tree {
        let mut t = Tree::new(PathBuf::from("r"));
        let a = t
            .append_children(
                0,
                &[e(true, 0), e(true, 0), e(false, 500), e(false, 1)],
                b"x",
            )
            .unwrap();
        let sizes: Vec<NewEntry> = (1..=40).map(|i| e(false, i * 10)).collect();
        t.append_children(a, &sizes, b"x").unwrap();
        let b = t
            .append_children(a + 1, &[e(false, 300), e(true, 0)], b"x")
            .unwrap();
        t.append_children(b + 1, &[e(false, 200), e(false, 100)], b"x")
            .unwrap();
        t
    }

    #[test]
    fn tiles_are_inside_and_disjoint() {
        let t = sample();
        let o = LayoutOptions {
            width: 800.0,
            height: 600.0,
            ..Default::default()
        };
        let l = compute(&t, None, 0, &o);
        assert!(l.tiles.len() > 10);
        assert_eq!(l.root_weight, t.node(0).alloc);
        let eps = 0.01;
        for t in &l.tiles {
            assert!(t.x >= -eps && t.y >= -eps, "{t:?}");
            assert!(
                t.x + t.w <= 800.0 + eps && t.y + t.h <= 600.0 + eps,
                "{t:?}"
            );
            assert!(t.w >= 0.0 && t.h >= 0.0);
        }
        // Leaves must not overlap each other.
        let leaves: Vec<&Tile> = l.tiles.iter().filter(|t| t.kind != TileKind::Dir).collect();
        for (i, a) in leaves.iter().enumerate() {
            for b in &leaves[i + 1..] {
                let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
                let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
                assert!(ox <= eps || oy <= eps, "overlap {a:?} {b:?}");
            }
        }
        // Leaf area is proportional to weight within one parent.
        let files: Vec<&&Tile> = leaves
            .iter()
            .filter(|t| t.kind == TileKind::File && t.depth == 1)
            .collect();
        let big = files.iter().find(|t| t.weight == 500).unwrap();
        let dirs: Vec<&Tile> = l
            .tiles
            .iter()
            .filter(|t| t.kind == TileKind::Dir && t.depth == 1)
            .collect();
        let d = dirs.iter().find(|t| t.weight == 600).unwrap();
        let ratio = (big.w * big.h) / (d.w * d.h);
        assert!((ratio - 500.0 / 600.0).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn hit_test_finds_deepest() {
        let t = sample();
        let l = compute(&t, None, 0, &LayoutOptions::default());
        let leaf = l
            .tiles
            .iter()
            .rposition(|t| t.kind == TileKind::File)
            .unwrap();
        let tile = l.tiles[leaf];
        assert_eq!(
            l.hit(tile.x + tile.w / 2.0, tile.y + tile.h / 2.0),
            Some(leaf)
        );
        assert_eq!(l.hit(-5.0, -5.0), None);
    }

    #[test]
    fn free_space_and_small_viewports() {
        let t = sample();
        let o = LayoutOptions {
            free_space: 5000,
            ..Default::default()
        };
        let l = compute(&t, None, 0, &o);
        assert_eq!(
            l.tiles.iter().filter(|t| t.kind == TileKind::Free).count(),
            1
        );
        let tiny = LayoutOptions {
            width: 2.0,
            height: 2.0,
            ..Default::default()
        };
        assert!(compute(&t, None, 0, &tiny).tiles.is_empty());
        assert!(compute(&t, None, 9999, &LayoutOptions::default())
            .tiles
            .is_empty());
    }

    #[test]
    fn filtered_weights() {
        let t = sample();
        let f = crate::Filter::parse(">250b", 0).unwrap();
        let mut w = Vec::new();
        f.compute_weights(&t, Metric::Logical, &mut w);
        // 500 + 300 + files >= 250 among 10..=400 step 10 (250..=400).
        let expect: u64 = 500 + 300 + (25..=40).map(|i| i * 10).sum::<u64>();
        assert_eq!(w[0], expect);
        let l = compute(&t, Some(&w), 0, &LayoutOptions::default());
        assert_eq!(l.root_weight, expect);
        assert!(l.tiles.iter().all(|t| t.weight > 0));
    }
}
