//! the look, a dark theme w/ the icon's red as the accent

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};

pub const ACCENT: Color32 = Color32::from_rgb(232, 64, 87);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(150, 38, 62);
pub const GOOD: Color32 = Color32::from_rgb(92, 204, 140);
pub const WARN: Color32 = Color32::from_rgb(240, 186, 82);
pub const BAD: Color32 = Color32::from_rgb(242, 98, 98);

pub const BG: Color32 = Color32::from_rgb(18, 18, 22);
pub const PANEL: Color32 = Color32::from_rgb(24, 24, 30);
pub const CARD: Color32 = Color32::from_rgb(32, 32, 40);
pub const CARD_HOVER: Color32 = Color32::from_rgb(40, 40, 50);
pub const BORDER: Color32 = Color32::from_rgb(52, 52, 64);
pub const TEXT: Color32 = Color32::from_rgb(226, 226, 234);
pub const WEAK: Color32 = Color32::from_rgb(140, 140, 156);

/// colors for the code / literals / unreached bar
pub const CODE: Color32 = Color32::from_rgb(232, 64, 87);
pub const LITERAL: Color32 = Color32::from_rgb(110, 140, 240);
pub const UNREACHED: Color32 = Color32::from_rgb(60, 60, 74);

/// room between widgets, the root ui goes w/o it so panels sit flush
pub const SPACING: egui::Vec2 = egui::vec2(8.0, 8.0);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = SPACING;
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.window_margin = Margin::same(12);

        let v = &mut style.visuals;
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.extreme_bg_color = BG;
        v.faint_bg_color = Color32::from_rgb(28, 28, 35);
        v.override_text_color = None;
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT_DIM;
        v.selection.stroke = Stroke::new(1.0, TEXT);
        v.window_stroke = Stroke::new(1.0, BORDER);

        let radius = CornerRadius::same(6);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = radius;
        w.inactive.bg_fill = Color32::from_rgb(44, 44, 54);
        w.inactive.weak_bg_fill = Color32::from_rgb(44, 44, 54);
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = radius;
        w.hovered.bg_fill = Color32::from_rgb(58, 58, 70);
        w.hovered.weak_bg_fill = Color32::from_rgb(58, 58, 70);
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(80, 80, 96));
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.hovered.corner_radius = radius;
        w.active.bg_fill = ACCENT_DIM;
        w.active.weak_bg_fill = ACCENT_DIM;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.active.corner_radius = radius;
        w.open.corner_radius = radius;
    });
}

/// a rounded box to group things in
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
}
