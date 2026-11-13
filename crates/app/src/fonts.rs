//! Uses the platform's own UI font so the app feels native.
//!
//! Wide-coverage fallbacks (CJK, Hangul, Indic, symbols) are tens of
//! megabytes, so they are only attached the first time a file name actually
//! needs them, and then memory-mapped rather than read.

use egui::{FontData, FontDefinitions, FontFamily};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(windows)]
fn font_dirs() -> Vec<PathBuf> {
    let win = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    vec![PathBuf::from(win).join("Fonts")]
}

#[cfg(not(windows))]
fn font_dirs() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ]
}

#[cfg(windows)]
mod names {
    pub const REGULAR: &[&str] = &["segoeui.ttf", "tahoma.ttf", "arial.ttf"];
    pub const BOLD: &[&str] = &["seguisb.ttf", "segoeuib.ttf", "tahomabd.ttf", "arialbd.ttf"];
    /// Per group: `(first match only, candidates)`.
    pub const GROUPS: [(bool, &[&str]); 3] = [
        (
            true,
            &["msyh.ttc", "YuGothR.ttc", "msgothic.ttc", "simsun.ttc"],
        ),
        (true, &["malgun.ttf"]),
        (
            false,
            &[
                "seguisym.ttf",
                "Nirmala.ttc",
                "Nirmala.ttf",
                "leelawui.ttf",
                "ebrima.ttf",
                "gadugi.ttf",
            ],
        ),
    ];
}

#[cfg(not(windows))]
mod names {
    pub const REGULAR: &[&str] = &[
        "truetype/ubuntu/Ubuntu-R.ttf",
        "truetype/ubuntu/Ubuntu[wdth,wght].ttf",
        "truetype/noto/NotoSans-Regular.ttf",
        "truetype/cantarell/Cantarell-Regular.ttf",
        "truetype/dejavu/DejaVuSans.ttf",
        "truetype/liberation/LiberationSans-Regular.ttf",
        "truetype/liberation2/LiberationSans-Regular.ttf",
    ];
    pub const BOLD: &[&str] = &[
        "truetype/ubuntu/Ubuntu-M.ttf",
        "truetype/noto/NotoSans-SemiBold.ttf",
        "truetype/noto/NotoSans-Bold.ttf",
        "truetype/dejavu/DejaVuSans-Bold.ttf",
        "truetype/liberation/LiberationSans-Bold.ttf",
        "truetype/liberation2/LiberationSans-Bold.ttf",
    ];
    const CJK: &[&str] = &[
        "opentype/noto/NotoSansCJK-Regular.ttc",
        "truetype/noto/NotoSansCJK-Regular.ttc",
        "truetype/droid/DroidSansFallbackFull.ttf",
        "truetype/wqy/wqy-microhei.ttc",
        "opentype/unifont/unifont.otf",
    ];
    pub const GROUPS: [(bool, &[&str]); 3] = [
        (true, CJK),
        (true, CJK),
        (
            false,
            &[
                "truetype/dejavu/DejaVuSans.ttf",
                "truetype/freefont/FreeSans.ttf",
                "truetype/noto/NotoSansSymbols2-Regular.ttf",
            ],
        ),
    ];
}

const CJK: usize = 0;
const HANGUL: usize = 1;
const OTHER: usize = 2;

/// Which fallback group, if any, a character needs.
fn group_of(c: char) -> Option<usize> {
    match c as u32 {
        0..=0x058F => None,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Some(HANGUL),
        0x2E80..=0xA4CF
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFFEF
        | 0x20000..=0x3FFFF => Some(CJK),
        // General punctuation and typographic symbols are in the base font.
        0x2000..=0x206F => None,
        _ => Some(OTHER),
    }
}

fn read_first(candidates: &[&str]) -> Option<Vec<u8>> {
    let dirs = font_dirs();
    candidates
        .iter()
        .flat_map(|n| dirs.iter().map(move |d| d.join(n)))
        .find_map(|p| std::fs::read(p).ok())
}

/// Maps a font file for the life of the process. Only the pages a glyph
/// lookup touches are ever brought into memory.
fn map_static(path: &std::path::Path) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: system font files are not truncated while in use; the mapping is
    // read-only and intentionally leaked so the slice is valid forever.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let leaked: &'static memmap2::Mmap = Box::leak(Box::new(map));
    Some(&leaked[..])
}

pub struct Fonts {
    defs: FontDefinitions,
    requested: [bool; 3],
    mapped: HashSet<PathBuf>,
}

impl Fonts {
    pub fn install(ctx: &egui::Context) -> Fonts {
        let mut defs = FontDefinitions::default();
        if let Some(bytes) = read_first(names::REGULAR) {
            defs.font_data
                .insert("ui".into(), Arc::new(FontData::from_owned(bytes)));
            defs.families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "ui".into());
        }
        let mut semi = defs
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        if let Some(bytes) = read_first(names::BOLD) {
            defs.font_data
                .insert("ui-bold".into(), Arc::new(FontData::from_owned(bytes)));
            semi.insert(0, "ui-bold".into());
        }
        defs.families.insert(crate::theme::semibold(), semi);
        ctx.set_fonts(defs.clone());
        Fonts {
            defs,
            requested: [false; 3],
            mapped: HashSet::new(),
        }
    }

    /// Makes sure every character of `text` can be drawn, attaching fallback
    /// fonts on first need. Cheap for ASCII.
    pub fn ensure(&mut self, ctx: &egui::Context, text: &str) {
        if text.is_ascii() || self.requested == [true; 3] {
            return;
        }
        let mut changed = false;
        for c in text.chars() {
            let Some(g) = group_of(c) else { continue };
            if !self.requested[g] {
                self.requested[g] = true;
                changed |= self.attach(g);
            }
        }
        if changed {
            ctx.set_fonts(self.defs.clone());
            ctx.request_repaint();
        }
    }

    fn attach(&mut self, group: usize) -> bool {
        let (first_only, candidates) = names::GROUPS[group];
        let dirs = font_dirs();
        let mut added = false;
        for name in candidates {
            let Some(path) = dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()) else {
                continue;
            };
            let mut hit = self.mapped.contains(&path);
            if !hit {
                if let Some(bytes) = map_static(&path) {
                    let key = format!("fallback-{}", self.mapped.len());
                    self.defs
                        .font_data
                        .insert(key.clone(), Arc::new(FontData::from_static(bytes)));
                    for family in self.defs.families.values_mut() {
                        family.push(key.clone());
                    }
                    self.mapped.insert(path);
                    hit = true;
                    added = true;
                }
            }
            if hit && first_only {
                break;
            }
        }
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups() {
        assert_eq!(group_of('a'), None);
        assert_eq!(group_of('é'), None);
        assert_eq!(group_of('Я'), None);
        assert_eq!(group_of('…'), None);
        assert_eq!(group_of('日'), Some(CJK));
        assert_eq!(group_of('ア'), Some(CJK));
        assert_eq!(group_of('한'), Some(HANGUL));
        assert_eq!(group_of('ก'), Some(OTHER));
    }
}
