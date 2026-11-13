//! Small vector icons drawn with the painter, so they are crisp at any scale
//! and never depend on a glyph being present in a font.

use egui::{pos2, vec2, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui};

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Back,
    Forward,
    Up,
    Home,
    Refresh,
    Stop,
    Plus,
    Minus,
    Close,

    Disk,
    Folder,
}

/// Paints `icon` centred in `rect`. Icons are designed on a 16-unit grid.
pub fn paint(p: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height()) / 16.0;
    let o = rect.center() - vec2(8.0 * s, 8.0 * s);
    let pt = |x: f32, y: f32| -> Pos2 { pos2(o.x + x * s, o.y + y * s) };
    let stroke = Stroke::new(1.6 * s.max(0.8), color);
    let line = |pts: &[(f32, f32)]| {
        p.add(Shape::line(
            pts.iter().map(|&(x, y)| pt(x, y)).collect(),
            stroke,
        ));
    };
    match icon {
        Icon::Back => line(&[(10.0, 3.0), (5.0, 8.0), (10.0, 13.0)]),
        Icon::Forward => line(&[(6.0, 3.0), (11.0, 8.0), (6.0, 13.0)]),
        Icon::Up => line(&[(3.0, 10.5), (8.0, 5.5), (13.0, 10.5)]),
        Icon::Home => {
            line(&[(2.5, 8.0), (8.0, 2.8), (13.5, 8.0)]);
            line(&[(4.2, 7.0), (4.2, 13.0), (11.8, 13.0), (11.8, 7.0)]);
        }
        Icon::Refresh => {
            let n = 20;
            let pts: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let a = (-50.0 + 290.0 * i as f32 / n as f32).to_radians();
                    pt(8.0 + 5.0 * a.cos(), 8.0 + 5.0 * a.sin())
                })
                .collect();
            p.add(Shape::line(pts, stroke));
            line(&[(11.2, 1.2), (11.4, 4.3), (14.4, 3.9)]);
        }
        Icon::Stop => {
            p.rect_filled(
                Rect::from_min_max(pt(3.5, 3.5), pt(12.5, 12.5)),
                1.5 * s,
                color,
            );
        }
        Icon::Plus => {
            line(&[(3.5, 8.0), (12.5, 8.0)]);
            line(&[(8.0, 3.5), (8.0, 12.5)]);
        }
        Icon::Minus => line(&[(3.5, 8.0), (12.5, 8.0)]),
        Icon::Close => {
            line(&[(4.0, 4.0), (12.0, 12.0)]);
            line(&[(12.0, 4.0), (4.0, 12.0)]);
        }
        Icon::Disk => {
            p.rect_stroke(
                Rect::from_min_max(pt(2.5, 4.5), pt(13.5, 11.5)),
                2.0 * s,
                stroke,
                egui::StrokeKind::Middle,
            );
            p.circle_filled(pt(10.8, 8.0), 1.1 * s, color);
            line(&[(4.8, 8.0), (7.6, 8.0)]);
        }
        Icon::Folder => {
            p.add(Shape::closed_line(
                [
                    (2.0, 4.0),
                    (6.2, 4.0),
                    (7.8, 5.8),
                    (14.0, 5.8),
                    (14.0, 12.5),
                    (2.0, 12.5),
                ]
                .iter()
                .map(|&(x, y)| pt(x, y))
                .collect(),
                stroke,
            ));
        }
    }
}

/// A square, frameless icon button with a hover tooltip.
pub fn button(ui: &mut Ui, icon: Icon, tip: &str, enabled: bool) -> Response {
    button_impl(ui, icon, tip, enabled, false)
}

/// An icon button that stays lit while `on`.
pub fn toggle(ui: &mut Ui, icon: Icon, tip: &str, on: bool) -> Response {
    button_impl(ui, icon, tip, true, on)
}

fn button_impl(ui: &mut Ui, icon: Icon, tip: &str, enabled: bool, on: bool) -> Response {
    let size = vec2(30.0, 28.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        if on {
            ui.painter().rect_filled(rect, 6.0, v.selection.bg_fill);
        }
        let color = if on {
            v.selection.stroke.color
        } else if !enabled {
            v.widgets.noninteractive.fg_stroke.color.gamma_multiply(0.3)
        } else if resp.hovered() {
            v.widgets.hovered.fg_stroke.color
        } else {
            v.widgets.inactive.fg_stroke.color.gamma_multiply(0.8)
        };
        if enabled && resp.hovered() {
            let bg = if resp.is_pointer_button_down_on() {
                v.widgets.active.bg_fill
            } else {
                v.widgets.hovered.bg_fill
            };
            ui.painter().rect_filled(rect, 6.0, bg);
        }
        paint(
            ui.painter(),
            Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
            icon,
            color,
        );
    }
    resp.on_hover_text(tip)
}
