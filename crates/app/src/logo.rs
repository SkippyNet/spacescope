//! The app mark: a tiny treemap. One definition drives the window icon, the
//! exported .ico/.png and the start screen.

use egui::{Color32, Rect};

/// `(x0, y0, x1, y1, rgb)` in the unit square.
const BLOCKS: [(f32, f32, f32, f32, u32); 5] = [
    (0.15, 0.15, 0.53, 0.85, 0x62acf3),
    (0.57, 0.15, 0.85, 0.47, 0xee6f6b),
    (0.57, 0.51, 0.70, 0.85, 0x72cf83),
    (0.74, 0.51, 0.85, 0.66, 0xf0c355),
    (0.74, 0.70, 0.85, 0.85, 0xb692f0),
];
const BACK: u32 = 0x171b23;

fn in_round_rect(x: f32, y: f32, r: (f32, f32, f32, f32), rad: f32) -> bool {
    let (x0, y0, x1, y1) = r;
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + rad, x1 - rad);
    let cy = y.clamp(y0 + rad, y1 - rad);
    (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad
}

/// Rasterises the mark with 4x4 supersampling. Returns RGBA8.
pub fn rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    const SS: u32 = 4;
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if !in_round_rect(x, y, (0.02, 0.02, 0.98, 0.98), 0.22) {
                        continue;
                    }
                    let mut c = BACK;
                    for &(x0, y0, x1, y1, col) in &BLOCKS {
                        if in_round_rect(x, y, (x0, y0, x1, y1), 0.045) {
                            c = col;
                            break;
                        }
                    }
                    r += (c >> 16) & 0xff;
                    g += (c >> 8) & 0xff;
                    b += c & 0xff;
                    a += 255;
                }
            }
            let n = SS * SS;
            let i = ((py * size + px) * 4) as usize;
            let cover = (a / 255).max(1);
            out[i] = (r / cover) as u8;
            out[i + 1] = (g / cover) as u8;
            out[i + 2] = (b / cover) as u8;
            out[i + 3] = (a / n) as u8;
        }
    }
    out
}

/// Paints the mark with the painter (vector, any size).
pub fn paint(p: &egui::Painter, rect: Rect) {
    let s = rect.width();
    p.rect_filled(rect, s * 0.22, crate::theme::rgb(BACK));
    for &(x0, y0, x1, y1, col) in &BLOCKS {
        let r = Rect::from_min_max(
            rect.min + egui::vec2(x0 * s, y0 * s),
            rect.min + egui::vec2(x1 * s, y1 * s),
        );
        p.rect_filled(r, s * 0.045, crate::theme::rgb(col));
    }
    let _ = Color32::WHITE;
}

/// Writes `spacescope.png` and `spacescope.ico` into `dir`.
pub fn write_assets(dir: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let size = 256u32;
    let img = image::RgbaImage::from_raw(size, size, rgba(size)).expect("buffer size");
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(std::io::Error::other)?;
    std::fs::write(dir.join("spacescope.png"), &png)?;

    // ICO container holding the PNG (supported since Windows Vista).
    let mut ico = Vec::new();
    ico.write_all(&[0, 0, 1, 0, 1, 0])?;
    ico.write_all(&[0, 0, 0, 0])?; // 256x256, no palette
    ico.write_all(&1u16.to_le_bytes())?;
    ico.write_all(&32u16.to_le_bytes())?;
    ico.write_all(&(png.len() as u32).to_le_bytes())?;
    ico.write_all(&22u32.to_le_bytes())?;
    ico.write_all(&png)?;
    std::fs::write(dir.join("spacescope.ico"), ico)
}
