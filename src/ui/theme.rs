use egui::{self, Color32, Context, Rounding, Stroke, Visuals};

pub const BG: Color32 = Color32::from_rgb(9, 13, 22);
pub const SIDEBAR: Color32 = Color32::from_rgb(14, 19, 31);
pub const PANEL: Color32 = Color32::from_rgb(18, 25, 40);
pub const PANEL_ALT: Color32 = Color32::from_rgb(23, 32, 50);
pub const BORDER: Color32 = Color32::from_rgb(39, 52, 77);
pub const MUTED: Color32 = Color32::from_rgb(143, 157, 181);
pub const TEXT: Color32 = Color32::from_rgb(235, 240, 250);
pub const SUCCESS: Color32 = Color32::from_rgb(50, 204, 147);
pub const WARNING: Color32 = Color32::from_rgb(247, 181, 72);
pub const DANGER: Color32 = Color32::from_rgb(242, 102, 119);
pub const BLUE: Color32 = Color32::from_rgb(85, 160, 255);

pub fn apply(context: &Context, accent: Color32, dark_mode: bool) {
    let mut visuals = if dark_mode {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.override_text_color = Some(if dark_mode {
        TEXT
    } else {
        Color32::from_rgb(34, 41, 56)
    });
    visuals.panel_fill = if dark_mode {
        BG
    } else {
        Color32::from_rgb(244, 247, 252)
    };
    visuals.window_fill = if dark_mode { PANEL } else { Color32::WHITE };
    visuals.faint_bg_color = if dark_mode { PANEL } else { Color32::from_rgb(235, 240, 247) };
    visuals.extreme_bg_color = if dark_mode { SIDEBAR } else { Color32::from_rgb(225, 231, 241) };
    visuals.selection.bg_fill = accent.linear_multiply(0.28);
    visuals.selection.stroke = Stroke::new(1.0_f32, accent);
    visuals.widgets.noninteractive.bg_fill = visuals.panel_fill;
    visuals.widgets.noninteractive.fg_stroke.color = visuals.override_text_color.unwrap_or(TEXT);
    visuals.widgets.inactive.bg_fill = if dark_mode { PANEL } else { Color32::WHITE };
    visuals.widgets.inactive.fg_stroke.color = visuals.override_text_color.unwrap_or(TEXT);
    visuals.widgets.hovered.bg_fill = accent.linear_multiply(0.2);
    visuals.widgets.hovered.fg_stroke.color = visuals.override_text_color.unwrap_or(TEXT);
    visuals.widgets.active.bg_fill = accent.linear_multiply(0.34);
    visuals.widgets.active.fg_stroke.color = Color32::WHITE;
    visuals.window_rounding = Rounding::same(14.0);
    visuals.menu_rounding = Rounding::same(10.0);
    context.set_visuals(visuals);
    context.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.visuals.window_shadow = egui::Shadow {
            offset: egui::vec2(0.0, 12.0),
            blur: 32.0,
            spread: 0.0,
            color: Color32::from_black_alpha(80),
        };
    });
}

pub fn status_color(status: crate::models::DownloadStatus) -> Color32 {
    use crate::models::DownloadStatus;
    match status {
        DownloadStatus::Queued => BLUE,
        DownloadStatus::Downloading => Color32::from_rgb(113, 137, 255),
        DownloadStatus::Paused => WARNING,
        DownloadStatus::Completed => SUCCESS,
        DownloadStatus::Failed => DANGER,
        DownloadStatus::Cancelled => MUTED,
    }
}

pub fn card_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(Rounding::same(14.0))
        .inner_margin(egui::Margin::same(16.0))
}
