//! The start screen: pick a drive or folder.

use crate::app::{Action, App};
use crate::icons::{self, Icon};
use crate::platform;
use crate::theme::{self, mix, Palette};
use crate::treemap::galley;
use egui::{
    pos2, vec2, Context, FontId, Key, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
};
use spacescope_core::fmt::human_size;
use std::path::PathBuf;

const CARD_H: f32 = 88.0;
const GAP: f32 = 12.0;

struct Card<'a> {
    icon: Icon,
    title: &'a str,
    subtitle: &'a str,
    /// `(used fraction, caption)`.
    usage: Option<(f32, String)>,
}

fn card(ui: &mut Ui, pal: &Palette, width: f32, c: &Card<'_>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, CARD_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = ui.painter();
    let hot = resp.hovered();
    let fill = if hot {
        mix(pal.panel, pal.accent, 0.07)
    } else {
        pal.panel
    };
    let edge = if hot {
        mix(pal.border, pal.accent, 0.7)
    } else {
        pal.border
    };
    p.rect(
        rect,
        10.0,
        fill,
        Stroke::new(1.0_f32, edge),
        StrokeKind::Inside,
    );

    let badge = Rect::from_min_size(rect.min + vec2(16.0, 16.0), vec2(36.0, 36.0));
    p.rect_filled(
        badge,
        9.0,
        pal.accent
            .gamma_multiply(if pal.dark { 0.16 } else { 0.12 }),
    );
    icons::paint(
        p,
        Rect::from_center_size(badge.center(), vec2(18.0, 18.0)),
        c.icon,
        pal.accent,
    );

    let left = badge.right() + 14.0;
    let text_w = rect.right() - 16.0 - left;
    let title = galley(
        p,
        c.title,
        FontId::new(15.0, theme::semibold()),
        pal.text,
        text_w,
    );
    let sub = galley(
        p,
        c.subtitle,
        FontId::proportional(12.0),
        pal.text_dim,
        text_w,
    );
    match &c.usage {
        Some((frac, caption)) => {
            p.galley(pos2(left, rect.top() + 14.0), title, pal.text);
            let sub_w = sub.size().x;
            p.galley(
                pos2(rect.right() - 16.0 - sub_w, rect.top() + 17.0),
                sub,
                pal.text_dim,
            );
            let bar = Rect::from_min_size(pos2(left, rect.top() + 43.0), vec2(text_w, 6.0));
            p.rect_filled(bar, 3.0, mix(pal.panel, pal.text_dim, 0.22));
            let color = if *frac > 0.92 {
                pal.danger
            } else if *frac > 0.8 {
                pal.warn
            } else {
                pal.accent
            };
            let mut used = bar;
            used.set_width((bar.width() * frac.clamp(0.0, 1.0)).max(6.0));
            p.rect_filled(used, 3.0, color);
            let cap = galley(p, caption, FontId::proportional(12.0), pal.text_dim, text_w);
            p.galley(pos2(left, rect.top() + 57.0), cap, pal.text_dim);
        }
        None => {
            p.galley(pos2(left, rect.top() + 23.0), title, pal.text);
            p.galley(pos2(left, rect.top() + 45.0), sub, pal.text_dim);
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn section(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(22.0);
    ui.label(
        RichText::new(text)
            .size(11.0)
            .color(pal.text_dim)
            .family(theme::semibold()),
    );
    ui.add_space(6.0);
}

impl App {
    pub(crate) fn start_screen(&mut self, ctx: &Context, pal: &Palette, out: &mut Vec<Action>) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    .inner_margin(Margin::same(0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let full = ui.available_width();
                        let col = (full - 48.0).clamp(300.0, 780.0);
                        let top = (ui.available_height() * 0.09).clamp(20.0, 80.0);
                        ui.add_space(top);
                        ui.horizontal(|ui| {
                            ui.add_space(((full - col) * 0.5).max(0.0));
                            ui.vertical(|ui| {
                                ui.set_width(col);
                                self.start_content(ui, pal, col, out);
                            });
                        });
                        ui.add_space(32.0);
                    });
            });
    }

    fn start_content(&mut self, ui: &mut Ui, pal: &Palette, col: f32, out: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(46.0, 46.0), Sense::hover());
            crate::logo::paint(ui.painter(), r);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new(crate::APP_NAME)
                        .family(theme::semibold())
                        .size(26.0)
                        .color(pal.text),
                );
                ui.label(
                    RichText::new("See what is filling your disk.")
                        .size(13.5)
                        .color(pal.text_dim),
                );
            });
        });

        let cols = if col >= 600.0 { 2 } else { 1 };
        let card_w = (col - GAP * (cols as f32 - 1.0)) / cols as f32;

        section(ui, pal, "DRIVES");
        let drives = self.drives.clone();
        if drives.is_empty() && self.drives_loaded {
            ui.label(RichText::new("No drives found. Choose a folder below.").color(pal.text_dim));
        }
        for row in drives.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for d in row {
                    let mount = d.mount.display().to_string();
                    let short = mount.trim_end_matches('\\').to_string();
                    let title = if cfg!(windows) {
                        let label = if d.label.is_empty() {
                            "Local Disk"
                        } else {
                            d.label.as_str()
                        };
                        format!("{label} ({short})")
                    } else {
                        mount.clone()
                    };
                    let used = d.total.saturating_sub(d.free);
                    let frac = used as f32 / d.total.max(1) as f32;
                    let caption = format!("{} free of {}", human_size(d.free), human_size(d.total));
                    let sub = if d.removable {
                        format!("{} · removable", d.fs)
                    } else {
                        d.fs.clone()
                    };
                    let c = Card {
                        icon: Icon::Disk,
                        title: &title,
                        subtitle: &sub,
                        usage: Some((frac, caption)),
                    };
                    if card(ui, pal, card_w, &c).clicked() {
                        out.push(Action::Scan(d.mount.clone()));
                    }
                }
            });
            ui.add_space(GAP - 6.0);
        }

        section(ui, pal, "FOLDERS");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let home = platform::home_dir();
            let n = if home.is_some() && cols == 2 {
                2.0
            } else {
                1.0
            };
            let w = (col - GAP * (n - 1.0)) / n;
            if let (Some(home), true) = (&home, cols == 2) {
                let path = home.display().to_string();
                let c = Card {
                    icon: Icon::Home,
                    title: "Home folder",
                    subtitle: &path,
                    usage: None,
                };
                if card(ui, pal, w, &c).clicked() {
                    out.push(Action::Scan(home.clone()));
                }
            }
            let c = Card {
                icon: Icon::Folder,
                title: "Choose a folder…",
                subtitle: "Scan any folder, or drop one onto this window",
                usage: None,
            };
            if card(ui, pal, w, &c).clicked() {
                out.push(Action::Browse);
            }
        });
        if let (Some(home), 1) = (platform::home_dir(), cols) {
            ui.add_space(GAP - 6.0);
            let path = home.display().to_string();
            let c = Card {
                icon: Icon::Home,
                title: "Home folder",
                subtitle: &path,
                usage: None,
            };
            if card(ui, pal, col, &c).clicked() {
                out.push(Action::Scan(home));
            }
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let edit = egui::TextEdit::singleline(&mut self.path_input)
                .hint_text("Or type a path and press Enter")
                .margin(Margin::symmetric(10, 7));
            let resp = ui.add_sized([col - 88.0, 32.0], edit);
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let go = ui
                .add_sized([80.0, 32.0], egui::Button::new("Scan"))
                .clicked();
            let text = self.path_input.trim();
            if (enter || go) && !text.is_empty() {
                out.push(Action::Scan(PathBuf::from(text)));
            }
        });
    }
}
