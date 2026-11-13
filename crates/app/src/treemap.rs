//! Turns a layout into one batched mesh plus text labels.

use crate::theme::{darken, lighten, Palette};
use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::text::{LayoutJob, TextWrapping};
use egui::{pos2, vec2, Color32, FontId, Galley, Painter, Pos2, Rect};
use spacescope_core::fmt::human_size;
use spacescope_core::{kind, Layout, Tile, TileKind};
use std::sync::Arc;

pub const HEADER_H: f32 = 20.0;

pub fn tile_rect(t: &Tile, origin: Pos2) -> Rect {
    Rect::from_min_size(pos2(origin.x + t.x, origin.y + t.y), vec2(t.w, t.h))
}

pub fn tile_fill(t: &Tile, pal: &Palette) -> Color32 {
    match t.kind {
        TileKind::Dir => pal.frame(t.depth),
        TileKind::File => pal.cats[t.cat as usize % kind::COUNT],
        TileKind::DirCollapsed => pal.folder,
        TileKind::More => pal.more,
        TileKind::Free => pal.free_fill,
    }
}

/// Builds every tile into a single mesh: one draw call for the whole map.
/// Edges are snapped to physical pixels so the 1px gaps stay even.
pub fn build_mesh(layout: &Layout, origin: Pos2, ppp: f32, pal: &Palette) -> Mesh {
    let mut m = Mesh::default();
    m.vertices.reserve(layout.tiles.len() * 4);
    m.indices.reserve(layout.tiles.len() * 6);
    let snap = |v: f32| (v * ppp).round() / ppp;
    let gap = 1.0 / ppp;
    for t in &layout.tiles {
        let x0 = snap(origin.x + t.x);
        let y0 = snap(origin.y + t.y);
        let mut x1 = snap(origin.x + t.x + t.w);
        let mut y1 = snap(origin.y + t.y + t.h);
        if t.depth > 0 {
            if x1 - x0 > 2.5 * gap {
                x1 -= gap;
            }
            if y1 - y0 > 2.5 * gap {
                y1 -= gap;
            }
        }
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let base = tile_fill(t, pal);
        let (tl, br) = match t.kind {
            TileKind::File => (lighten(base, 0.14), darken(base, 0.18)),
            TileKind::DirCollapsed => (lighten(base, 0.09), darken(base, 0.14)),
            TileKind::More => (lighten(base, 0.04), darken(base, 0.07)),
            TileKind::Dir | TileKind::Free => (base, base),
        };
        let i = m.vertices.len() as u32;
        let v = |x: f32, y: f32, color: Color32| Vertex {
            pos: pos2(x, y),
            uv: WHITE_UV,
            color,
        };
        m.vertices.push(v(x0, y0, tl));
        m.vertices.push(v(x1, y0, base));
        m.vertices.push(v(x0, y1, base));
        m.vertices.push(v(x1, y1, br));
        m.indices
            .extend_from_slice(&[i, i + 1, i + 2, i + 1, i + 3, i + 2]);
    }
    m
}

/// Copy of `mesh` with rectangle `a` mapped onto rectangle `b` and faded.
pub fn remap(mesh: &Mesh, a: Rect, b: Rect, opacity: f32) -> Mesh {
    let sx = b.width() / a.width().max(1e-3);
    let sy = b.height() / a.height().max(1e-3);
    let mut m = mesh.clone();
    let fade = opacity < 0.999;
    for v in &mut m.vertices {
        v.pos.x = b.min.x + (v.pos.x - a.min.x) * sx;
        v.pos.y = b.min.y + (v.pos.y - a.min.y) * sy;
        if fade {
            v.color = v.color.gamma_multiply(opacity);
        }
    }
    m
}

/// Single-line text clipped to `max_w` with an ellipsis.
pub fn galley(p: &Painter, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping {
        max_width: max_w.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    p.layout_job(job)
}

pub fn paint_labels(p: &Painter, layout: &Layout, origin: Pos2, pal: &Palette, alpha: f32) {
    if alpha <= 0.01 {
        return;
    }
    let name_font = FontId::proportional(12.0);
    let size_font = FontId::proportional(10.5);
    let head_font = FontId::new(12.0, crate::theme::semibold());
    for (idx, text) in &layout.labels {
        let t = &layout.tiles[*idx as usize];
        let r = tile_rect(t, origin);
        if t.kind == TileKind::Dir {
            let color = pal.frame_text.gamma_multiply(alpha);
            let avail = r.width() - 12.0;
            let mut name_w = avail;
            if avail > 150.0 {
                let size = galley(
                    p,
                    &human_size(t.weight),
                    size_font.clone(),
                    pal.text_dim.gamma_multiply(alpha),
                    90.0,
                );
                let sw = size.size().x;
                let y = r.top() + (HEADER_H - size.size().y) * 0.5;
                p.galley(pos2(r.right() - 7.0 - sw, y), size, color);
                name_w = avail - sw - 12.0;
            }
            let g = galley(p, text, head_font.clone(), color, name_w);
            let y = r.top() + (HEADER_H - g.size().y) * 0.5;
            p.galley(pos2(r.left() + 6.0, y), g, color);
        } else {
            let base = tile_fill(t, pal);
            let color = if t.kind == TileKind::Free {
                pal.free_text
            } else if t.kind == TileKind::More {
                pal.text_dim
            } else {
                pal.text_on(base)
            };
            let g = galley(
                p,
                text,
                name_font.clone(),
                color.gamma_multiply(alpha),
                r.width() - 9.0,
            );
            let line = g.size().y;
            p.galley(pos2(r.left() + 5.0, r.top() + 3.0), g, color);
            if r.height() >= 2.0 * line + 6.0 && r.width() >= 44.0 {
                let s = galley(
                    p,
                    &human_size(t.weight),
                    size_font.clone(),
                    color.gamma_multiply(alpha * 0.7),
                    r.width() - 9.0,
                );
                p.galley(pos2(r.left() + 5.0, r.top() + 3.0 + line), s, color);
            }
        }
    }
}
