//! Colours and egui styling.

use egui::{Color32, CornerRadius, Stroke, Theme, Visuals};
use spacescope_core::kind;

pub const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub fn lighten(c: Color32, t: f32) -> Color32 {
    mix(c, Color32::WHITE, t)
}

pub fn darken(c: Color32, t: f32) -> Color32 {
    mix(c, Color32::BLACK, t)
}

pub fn luminance(c: Color32) -> f32 {
    (0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32) / 255.0
}

#[derive(Clone)]
pub struct Palette {
    pub dark: bool,
    /// Window background, visible in the gaps of the map.
    pub bg: Color32,
    pub panel: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub accent: Color32,
    pub warn: Color32,
    pub danger: Color32,
    /// Folder frame colour at the root and at depth.
    pub frame_lo: Color32,
    pub frame_hi: Color32,
    pub frame_text: Color32,
    pub cats: [Color32; kind::COUNT],
    pub folder: Color32,
    pub more: Color32,
    pub free_fill: Color32,
    pub free_text: Color32,
    pub on_light: Color32,
    pub on_dark: Color32,
    pub hover: Color32,
}

impl Palette {
    pub fn dark() -> Self {
        Palette {
            dark: true,
            bg: rgb(0x0c0e12),
            panel: rgb(0x14171d),
            raised: rgb(0x1c2029),
            border: rgb(0x2a2f3b),
            text: rgb(0xe7e9ee),
            text_dim: rgb(0x8d95a5),
            accent: rgb(0x6ea8fe),
            warn: rgb(0xf2c14e),
            danger: rgb(0xf0716c),
            frame_lo: rgb(0x181b22),
            frame_hi: rgb(0x353c4c),
            frame_text: rgb(0xcfd4de),
            cats: [
                rgb(0x8590a3), // other
                rgb(0xee6f6b), // video
                rgb(0xb692f0), // audio
                rgb(0xf0c355), // images
                rgb(0x62acf3), // documents
                rgb(0xef914f), // archives
                rgb(0x72cf83), // code
                rgb(0x45c7bf), // programs
                rgb(0xdc84bd), // data
            ],
            folder: rgb(0x4d5a75),
            more: rgb(0x2f3644),
            free_fill: rgb(0x14241d),
            free_text: rgb(0x58b98a),
            on_light: rgb(0x0b0d11),
            on_dark: rgb(0xe9ecf2),
            hover: Color32::WHITE,
        }
    }

    pub fn light() -> Self {
        Palette {
            dark: false,
            bg: rgb(0xe9ecf1),
            panel: rgb(0xfbfcfd),
            raised: rgb(0xffffff),
            border: rgb(0xd5dae3),
            text: rgb(0x1a1e26),
            text_dim: rgb(0x667085),
            accent: rgb(0x2f6fe4),
            warn: rgb(0xb7791f),
            danger: rgb(0xd64545),
            frame_lo: rgb(0xf4f6f9),
            frame_hi: rgb(0xc3cad6),
            frame_text: rgb(0x2a303b),
            cats: [
                rgb(0x9aa5b8),
                rgb(0xf07f7b),
                rgb(0xb99af2),
                rgb(0xf2c75e),
                rgb(0x6cb2f5),
                rgb(0xf19a5b),
                rgb(0x79d18a),
                rgb(0x50cbc3),
                rgb(0xe08fc4),
            ],
            folder: rgb(0x8793ab),
            more: rgb(0xcdd3dd),
            free_fill: rgb(0xdff1e7),
            free_text: rgb(0x2f8a5f),
            on_light: rgb(0x0f1217),
            on_dark: rgb(0xffffff),
            hover: rgb(0x0f1217),
        }
    }

    pub fn of(ctx: &egui::Context) -> Self {
        if ctx.style().visuals.dark_mode {
            Self::dark()
        } else {
            Self::light()
        }
    }

    /// Readable text colour on top of `fill`.
    pub fn text_on(&self, fill: Color32) -> Color32 {
        if luminance(fill) > 0.42 {
            self.on_light
        } else {
            self.on_dark
        }
    }

    pub fn frame(&self, depth: u8) -> Color32 {
        mix(self.frame_lo, self.frame_hi, depth.min(6) as f32 / 6.0)
    }

    fn visuals(&self) -> Visuals {
        let mut v = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        v.panel_fill = self.panel;
        v.window_fill = self.raised;
        v.extreme_bg_color = if self.dark {
            rgb(0x0e1014)
        } else {
            rgb(0xffffff)
        };
        v.faint_bg_color = self.raised;
        v.window_stroke = Stroke::new(1.0_f32, self.border);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self
            .accent
            .gamma_multiply(if self.dark { 0.35 } else { 0.25 });
        v.selection.stroke = Stroke::new(1.0_f32, self.accent);
        v.warn_fg_color = self.warn;
        v.error_fg_color = self.danger;

        let r = CornerRadius::same(6);
        let hover_bg = if self.dark {
            rgb(0x272c38)
        } else {
            rgb(0xe6eaf0)
        };
        let active_bg = if self.dark {
            rgb(0x313747)
        } else {
            rgb(0xd9dee7)
        };
        let idle_bg = if self.dark {
            rgb(0x1e222b)
        } else {
            rgb(0xeef1f5)
        };

        let w = &mut v.widgets;
        w.noninteractive.corner_radius = r;
        w.noninteractive.bg_fill = self.panel;
        w.noninteractive.weak_bg_fill = self.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, self.text);

        w.inactive.corner_radius = r;
        w.inactive.bg_fill = idle_bg;
        w.inactive.weak_bg_fill = idle_bg;
        w.inactive.bg_stroke = Stroke::new(1.0_f32, self.border);
        w.inactive.fg_stroke = Stroke::new(1.0_f32, self.text);

        w.hovered.corner_radius = r;
        w.hovered.bg_fill = hover_bg;
        w.hovered.weak_bg_fill = hover_bg;
        w.hovered.bg_stroke = Stroke::new(1.0_f32, mix(self.border, self.accent, 0.5));
        w.hovered.fg_stroke = Stroke::new(1.0_f32, self.text);
        w.hovered.expansion = 0.0;

        w.active.corner_radius = r;
        w.active.bg_fill = active_bg;
        w.active.weak_bg_fill = active_bg;
        w.active.bg_stroke = Stroke::new(1.0_f32, self.accent);
        w.active.fg_stroke = Stroke::new(1.0_f32, self.text);
        w.active.expansion = 0.0;

        w.open.corner_radius = r;
        w.open.bg_fill = active_bg;
        w.open.weak_bg_fill = active_bg;
        w.open.bg_stroke = Stroke::new(1.0_f32, self.border);
        w.open.fg_stroke = Stroke::new(1.0_f32, self.text);
        v
    }
}

/// Installs both themes and the shared spacing / type scale.
pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Dark, Palette::dark().visuals());
    ctx.set_visuals_of(Theme::Light, Palette::light().visuals());
    ctx.all_styles_mut(|s| {
        use egui::{FontFamily::Proportional, FontId, TextStyle};
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.interact_size.y = 28.0;
        s.interaction.tooltip_delay = 0.35;
        // Labels are chrome here, not documents: no text cursor, no selection.
        s.interaction.selectable_labels = false;
        s.text_styles
            .insert(TextStyle::Body, FontId::new(13.5, Proportional));
        s.text_styles
            .insert(TextStyle::Button, FontId::new(13.5, Proportional));
        s.text_styles
            .insert(TextStyle::Small, FontId::new(11.5, Proportional));
        s.text_styles
            .insert(TextStyle::Heading, FontId::new(20.0, semibold()));
    });
}

pub fn semibold() -> egui::FontFamily {
    egui::FontFamily::Name("semibold".into())
}
