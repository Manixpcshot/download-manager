//! Design tokens and reusable primitives for the Pulse desktop interface.
//!
//! Pulse is a compiled Rust/egui application: everything in this file is rasterized by the GPU
//! backend of `eframe`. There is no HTML, CSS, JavaScript or webview layer involved anywhere in
//! the interface, and the tokens below are plain `Color32`/`f32` values instead of stylesheets.

use egui::{
    self, Button, Color32, Context, FontFamily, FontId, Frame, Margin, RichText, Rounding, Shadow,
    Stroke, TextStyle, Visuals,
};

use crate::models::DownloadStatus;
use crate::notifications::NotificationKind;

/// Corner radius used by compact controls such as buttons and chips.
pub const RADIUS_SM: f32 = 8.0;
/// Corner radius used by inputs, pills and inner surfaces.
pub const RADIUS_MD: f32 = 12.0;
/// Corner radius used by cards, panels and popovers.
pub const RADIUS_LG: f32 = 16.0;

// ---------------------------------------------------------------------------------------------
// Dark palette (the default, "dark-first" look).
// ---------------------------------------------------------------------------------------------

const BG_DARK: Color32 = Color32::from_rgb(8, 11, 19);
const SIDEBAR_DARK: Color32 = Color32::from_rgb(12, 16, 26);
const PANEL_DARK: Color32 = Color32::from_rgb(18, 23, 36);
const PANEL_ALT_DARK: Color32 = Color32::from_rgb(24, 31, 47);
const INSET_DARK: Color32 = Color32::from_rgb(13, 18, 29);
const BORDER_DARK: Color32 = Color32::from_rgb(36, 46, 68);
const BORDER_STRONG_DARK: Color32 = Color32::from_rgb(52, 65, 94);
const TEXT_DARK: Color32 = Color32::from_rgb(234, 239, 250);
const MUTED_DARK: Color32 = Color32::from_rgb(140, 154, 180);
const FAINT_DARK: Color32 = Color32::from_rgb(98, 112, 138);

// ---------------------------------------------------------------------------------------------
// Light palette (opt-in through Settings, kept in sync with the dark tokens).
// ---------------------------------------------------------------------------------------------

const BG_LIGHT: Color32 = Color32::from_rgb(243, 245, 250);
const SIDEBAR_LIGHT: Color32 = Color32::from_rgb(236, 240, 248);
const PANEL_LIGHT: Color32 = Color32::from_rgb(255, 255, 255);
const PANEL_ALT_LIGHT: Color32 = Color32::from_rgb(245, 247, 252);
const INSET_LIGHT: Color32 = Color32::from_rgb(233, 237, 245);
const BORDER_LIGHT: Color32 = Color32::from_rgb(218, 224, 236);
const BORDER_STRONG_LIGHT: Color32 = Color32::from_rgb(196, 204, 220);
const TEXT_LIGHT: Color32 = Color32::from_rgb(26, 32, 46);
const MUTED_LIGHT: Color32 = Color32::from_rgb(100, 112, 134);
const FAINT_LIGHT: Color32 = Color32::from_rgb(140, 150, 170);

// ---------------------------------------------------------------------------------------------
// Semantic colors, shared by both palettes.
// ---------------------------------------------------------------------------------------------

const SUCCESS: Color32 = Color32::from_rgb(52, 205, 147);
const WARNING: Color32 = Color32::from_rgb(247, 181, 72);
const DANGER: Color32 = Color32::from_rgb(242, 102, 119);
const INFO: Color32 = Color32::from_rgb(85, 160, 255);

/// Resolved color tokens for one frame of the interface.
///
/// The struct is `Copy` on purpose: screens can keep a local copy while they mutate application
/// state, which keeps the borrow checker happy without cloning color values around.
#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub dark: bool,
    pub accent: Color32,
    pub bg: Color32,
    pub sidebar: Color32,
    pub panel: Color32,
    pub panel_alt: Color32,
    pub inset: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub info: Color32,
}

/// Build the palette for the requested theme and accent color.
pub fn tokens(dark: bool, accent: Color32) -> Tokens {
    if dark {
        Tokens {
            dark,
            accent,
            bg: BG_DARK,
            sidebar: SIDEBAR_DARK,
            panel: PANEL_DARK,
            panel_alt: PANEL_ALT_DARK,
            inset: INSET_DARK,
            border: BORDER_DARK,
            border_strong: BORDER_STRONG_DARK,
            text: TEXT_DARK,
            muted: MUTED_DARK,
            faint: FAINT_DARK,
            success: SUCCESS,
            warning: WARNING,
            danger: DANGER,
            info: INFO,
        }
    } else {
        Tokens {
            dark,
            accent,
            bg: BG_LIGHT,
            sidebar: SIDEBAR_LIGHT,
            panel: PANEL_LIGHT,
            panel_alt: PANEL_ALT_LIGHT,
            inset: INSET_LIGHT,
            border: BORDER_LIGHT,
            border_strong: BORDER_STRONG_LIGHT,
            text: TEXT_LIGHT,
            muted: MUTED_LIGHT,
            faint: FAINT_LIGHT,
            success: SUCCESS,
            warning: WARNING,
            danger: DANGER,
            info: INFO,
        }
    }
}

/// Transparent tint of a color, used for chips, pills and selected rows.
pub fn tint(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// Repeated vertical space used between cards and sections.
pub fn gap() -> f32 {
    12.0
}

impl Tokens {
    /// Semantic color for a download state.
    pub fn status(&self, status: DownloadStatus) -> Color32 {
        match status {
            DownloadStatus::Queued => self.info,
            DownloadStatus::Downloading => self.accent,
            DownloadStatus::Paused => self.warning,
            DownloadStatus::Completed => self.success,
            DownloadStatus::Failed => self.danger,
            DownloadStatus::Cancelled => self.faint,
        }
    }

    /// Card surface: the default container for grouped content.
    pub fn card(&self) -> Frame {
        Frame::none()
            .fill(self.panel)
            .stroke(Stroke::new(1.0_f32, self.border))
            .rounding(Rounding::same(RADIUS_LG))
            .inner_margin(Margin::symmetric(16.0, 14.0))
    }

    /// Inset surface: nested panels such as banners, empty states and progress tracks.
    pub fn inset_frame(&self) -> Frame {
        Frame::none()
            .fill(self.inset)
            .stroke(Stroke::new(1.0_f32, self.border))
            .rounding(Rounding::same(RADIUS_MD))
            .inner_margin(Margin::symmetric(14.0, 12.0))
    }

    /// Filled primary action button using the current accent color.
    pub fn primary_button(&self, text: impl Into<String>) -> Button<'_> {
        Button::new(RichText::new(text.into()).strong().color(Color32::WHITE))
            .fill(self.accent)
            .stroke(Stroke::new(1.0_f32, self.accent))
            .rounding(Rounding::same(RADIUS_SM))
            .min_size(egui::vec2(0.0, 30.0))
    }

    /// Neutral secondary button for the surrounding chrome.
    pub fn subtle_button(&self, text: impl Into<String>) -> Button<'_> {
        Button::new(RichText::new(text.into()).color(self.text))
            .fill(self.panel_alt)
            .stroke(Stroke::new(1.0_f32, self.border))
            .rounding(Rounding::same(RADIUS_SM))
            .min_size(egui::vec2(0.0, 30.0))
    }

    /// Rounded status pill tinted with the semantic color.
    pub fn pill(&self, text: impl Into<egui::WidgetText>, color: Color32) -> Button<'_> {
        Button::new(text.into())
            .fill(tint(color, if self.dark { 34 } else { 26 }))
            .stroke(Stroke::new(
                1.0_f32,
                tint(color, if self.dark { 96 } else { 80 }),
            ))
            .rounding(Rounding::same(999.0))
            .min_size(egui::vec2(0.0, 24.0))
    }

    /// Muted label text.
    pub fn muted_text(&self, text: impl Into<String>) -> RichText {
        RichText::new(text.into()).size(12.5).color(self.muted)
    }

    /// Smallest caption text, used for hints and fine print.
    pub fn faint_text(&self, text: impl Into<String>) -> RichText {
        RichText::new(text.into()).size(11.5).color(self.faint)
    }
}

/// Page title.
pub fn heading(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(22.0).strong()
}

/// Section or card title.
pub fn card_title(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(14.5).strong()
}

/// Large numeric value inside statistic cards.
pub fn metric(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(23.0).strong()
}

/// Status label with a leading bullet, colored with the semantic status color.
pub fn status_label(status: DownloadStatus, color: Color32) -> RichText {
    RichText::new(format!("●  {}", status.label()))
        .size(11.5)
        .color(color)
}

/// Accent color that represents the file type of a download.
pub fn file_color(extension: &str) -> Color32 {
    match extension {
        "ZIP" | "RAR" | "7Z" | "TAR" | "GZ" => Color32::from_rgb(242, 173, 73),
        "EXE" | "MSI" | "DMG" | "APPX" => Color32::from_rgb(89, 156, 255),
        "MP4" | "MOV" | "MKV" | "AVI" | "WEBM" => Color32::from_rgb(212, 111, 255),
        "MP3" | "WAV" | "FLAC" | "OGG" | "M4A" => Color32::from_rgb(78, 211, 177),
        "PDF" | "DOC" | "DOCX" | "TXT" | "MD" => Color32::from_rgb(241, 99, 116),
        "ISO" | "IMG" => Color32::from_rgb(126, 167, 255),
        _ => Color32::from_rgb(140, 157, 255),
    }
}

/// Accent color for a notification kind.
pub fn notification_color(kind: NotificationKind, tokens: &Tokens) -> Color32 {
    match kind {
        NotificationKind::Info => tokens.info,
        NotificationKind::Success => tokens.success,
        NotificationKind::Warning => tokens.warning,
        NotificationKind::Error => tokens.danger,
    }
}

fn surface_shadow(dark: bool) -> Shadow {
    Shadow {
        offset: egui::vec2(0.0, 10.0),
        blur: 28.0,
        spread: 0.0,
        color: if dark {
            Color32::from_black_alpha(96)
        } else {
            Color32::from_black_alpha(28)
        },
    }
}

/// Install the Pulse visual style on the egui context.
///
/// The look is dark-first: dark surfaces, violet accent, crisp 1 px borders, roomy spacing and
/// rounded 16 px cards. Light mode mirrors the same structure with lighter surfaces so the layout
/// stays identical when the user switches themes.
pub fn apply(context: &Context, accent: Color32, dark: bool) {
    let t = tokens(dark, accent);
    let mut visuals = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    visuals.override_text_color = Some(t.text);
    visuals.panel_fill = t.bg;
    visuals.window_fill = t.panel;
    visuals.window_stroke = Stroke::new(1.0_f32, t.border_strong);
    visuals.window_rounding = Rounding::same(RADIUS_LG);
    visuals.window_shadow = surface_shadow(dark);
    visuals.popup_shadow = surface_shadow(dark);
    visuals.menu_rounding = Rounding::same(RADIUS_MD);
    visuals.faint_bg_color = t.panel_alt;
    visuals.extreme_bg_color = t.inset;
    visuals.code_bg_color = t.inset;
    visuals.hyperlink_color = t.info;
    visuals.warn_fg_color = t.warning;
    visuals.error_fg_color = t.danger;
    visuals.selection.bg_fill = tint(accent, if dark { 70 } else { 52 });
    visuals.selection.stroke = Stroke::new(1.0_f32, accent);

    visuals.widgets.noninteractive.bg_fill = t.panel;
    visuals.widgets.noninteractive.weak_bg_fill = t.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, t.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.muted);
    visuals.widgets.noninteractive.rounding = Rounding::same(RADIUS_MD);
    visuals.widgets.noninteractive.expansion = 0.0;

    visuals.widgets.inactive.bg_fill = t.panel_alt;
    visuals.widgets.inactive.weak_bg_fill = t.panel_alt;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, t.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, t.text);
    visuals.widgets.inactive.rounding = Rounding::same(RADIUS_SM);
    visuals.widgets.inactive.expansion = 0.0;

    visuals.widgets.hovered.bg_fill = if dark { t.panel } else { t.panel_alt };
    visuals.widgets.hovered.weak_bg_fill = t.panel_alt;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, tint(accent, 140));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, t.text);
    visuals.widgets.hovered.rounding = Rounding::same(RADIUS_SM);
    visuals.widgets.hovered.expansion = 0.0;

    visuals.widgets.active.bg_fill = tint(accent, if dark { 90 } else { 70 });
    visuals.widgets.active.weak_bg_fill = tint(accent, if dark { 90 } else { 70 });
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, t.text);
    visuals.widgets.active.rounding = Rounding::same(RADIUS_SM);
    visuals.widgets.active.expansion = 0.0;

    visuals.widgets.open.bg_fill = t.panel_alt;
    visuals.widgets.open.weak_bg_fill = t.panel_alt;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, t.border_strong);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, t.text);
    visuals.widgets.open.rounding = Rounding::same(RADIUS_SM);
    visuals.widgets.open.expansion = 0.0;

    visuals.striped = false;
    visuals.slider_trailing_fill = true;

    context.set_visuals(visuals);
    context.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.window_margin = Margin::symmetric(18.0, 16.0);
        style.spacing.menu_margin = Margin::same(8.0);
        style.spacing.indent = 18.0;
        style.spacing.interact_size = egui::vec2(26.0, 26.0);
        style.spacing.slider_width = 190.0;
        style.spacing.combo_width = 170.0;
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(20.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}
