use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::{App, Hsla, Window, px, rgb};
use volant_app_core::model::ThemeMode as AppThemeMode;

#[derive(Clone, Copy)]
pub struct VolantColors {
    pub canvas: Hsla,
    pub titlebar: Hsla,
    pub surface: Hsla,
    pub rail: Hsla,
    pub sunken: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    pub selected_border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub primary: Hsla,
    pub primary_text: Hsla,
    pub current: Hsla,
    pub current_soft: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub scrim: Hsla,
}

impl VolantColors {
    pub fn resolve(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self {
                canvas: color(0xFFFFFF),
                titlebar: color(0xF7F8FA),
                surface: color(0xFFFFFF),
                rail: color(0xF7F8FA),
                sunken: color(0xF3F5F7),
                hover: color(0xF2F4F6),
                selected: color(0xEDF2FF),
                selected_border: color(0xB9CAF4),
                text: color(0x111417),
                muted: color(0x59626B),
                faint: color(0x657079),
                border: color(0xE1E4E7),
                border_strong: color(0xCDD3D8),
                primary: color(0x151A1E),
                primary_text: color(0xFFFFFF),
                current: color(0x2F6BFF),
                current_soft: color(0xEDF2FF),
                success: color(0x16765A),
                warning: color(0x94621C),
                danger: color(0xB3423D),
                scrim: Hsla::from(rgb(0xE9ECEF)).opacity(0.36),
            },
            ThemeMode::Dark => Self {
                canvas: color(0x101318),
                titlebar: color(0x0D1014),
                surface: color(0x15191F),
                rail: color(0x12161B),
                sunken: color(0x171C22),
                hover: color(0x1B2128),
                selected: color(0x202832),
                selected_border: color(0x334A72),
                text: color(0xEDF1F5),
                muted: color(0xA2ACB6),
                faint: color(0x87929C),
                border: color(0x222830),
                border_strong: color(0x34404C),
                primary: color(0x7299FF),
                primary_text: color(0x0D1422),
                current: color(0x2F6BFF),
                current_soft: color(0x202832),
                success: color(0x52D3A3),
                warning: color(0xE3A94F),
                danger: color(0xFF8078),
                scrim: Hsla::from(rgb(0x090B0E)).opacity(0.76),
            },
        }
    }
}

fn color(hex: u32) -> Hsla {
    Hsla::from(rgb(hex))
}

pub fn resolve_mode(preferred: AppThemeMode, window: &Window) -> ThemeMode {
    match preferred {
        AppThemeMode::Light => ThemeMode::Light,
        AppThemeMode::Dark => ThemeMode::Dark,
        AppThemeMode::System => ThemeMode::from(window.appearance()),
    }
}

pub fn apply(cx: &mut App, mode: ThemeMode) {
    Theme::change(mode, None, cx);
    let colors = VolantColors::resolve(mode);
    Theme::update(cx, |theme| {
        theme.background = colors.surface;
        theme.foreground = colors.text;
        theme.popover = colors.surface;
        theme.popover_foreground = colors.text;
        theme.primary = colors.primary;
        theme.primary_foreground = colors.primary_text;
        theme.primary_hover = colors.primary.opacity(0.88);
        theme.primary_active = colors.primary.opacity(0.78);
        theme.button_primary = colors.primary;
        theme.button_primary_hover = colors.primary.opacity(0.88);
        theme.button_primary_active = colors.primary.opacity(0.78);
        theme.button_primary_foreground = colors.primary_text;
        theme.secondary = colors.surface;
        theme.secondary_foreground = colors.text;
        theme.secondary_hover = colors.hover;
        theme.secondary_active = colors.sunken;
        theme.button_secondary = colors.surface;
        theme.button_secondary_hover = colors.hover;
        theme.button_secondary_active = colors.sunken;
        theme.button_secondary_foreground = colors.text;
        theme.button = colors.surface;
        theme.button_hover = colors.hover;
        theme.button_active = colors.sunken;
        theme.button_foreground = colors.text;
        theme.muted = colors.sunken;
        theme.muted_foreground = colors.muted;
        theme.accent = colors.current_soft;
        theme.accent_foreground = colors.text;
        theme.border = colors.border;
        theme.input = colors.border_strong;
        theme.ring = colors.current.opacity(0.55);
        theme.selection = colors.current_soft;
        theme.sidebar = colors.rail;
        theme.sidebar_foreground = colors.text;
        theme.sidebar_border = colors.border;
        theme.sidebar_accent = colors.selected;
        theme.sidebar_accent_foreground = colors.text;
        theme.sidebar_primary = colors.primary;
        theme.sidebar_primary_foreground = colors.primary_text;
        theme.title_bar = colors.titlebar;
        theme.title_bar_border = colors.border;
        theme.switch = colors.border_strong;
        theme.switch_thumb = colors.surface;
        theme.progress_bar = colors.current;
        theme.success = colors.success;
        theme.success_foreground = colors.surface;
        theme.warning = colors.warning;
        theme.warning_foreground = colors.surface;
        theme.danger = colors.danger;
        theme.danger_foreground = colors.surface;
        theme.overlay = colors.scrim;
        theme.scrollbar = colors.surface;
        theme.scrollbar_thumb = colors.border_strong;
        theme.scrollbar_thumb_hover = colors.muted;
        theme.radius = px(7.);
        theme.radius_lg = px(10.);
        theme.window_radius = px(10.);
        theme.shadow = false;
        theme.font_family = "Segoe UI Variable Text".into();
        theme.font_size = px(text::BASE);
        theme.mono_font_family = "Cascadia Mono".into();
        theme.mono_font_size = px(text::DATA);
    });
}

pub fn current(cx: &App) -> VolantColors {
    VolantColors::resolve(cx.theme().mode)
}

pub mod size {
    pub const TITLEBAR: f32 = 34.0;
    pub const RAIL: f32 = 196.0;
    pub const RAIL_ICON: f32 = 64.0;
    pub const INSPECTOR: f32 = 260.0;
    pub const MAIN_PAD: f32 = 22.0;
    pub const MAIN_GAP: f32 = 16.0;
    pub const SPEED_MIN: f32 = 70.0;
    pub const QUEUE_HEADER: f32 = 31.0;
    pub const TASK_ROW: f32 = 59.0;
    pub const PROGRESS_COL: f32 = 190.0;
    pub const SIZE_COL: f32 = 82.0;
    pub const STATUS_COL: f32 = 78.0;
    pub const ACTION_COL: f32 = 88.0;
    pub const ROW_GAP: f32 = 12.0;
    pub const ROW_PAD: f32 = 11.0;
    pub const SETTINGS_W: f32 = 700.0;
    pub const SETTINGS_H: f32 = 610.0;
    pub const SETTINGS_RAIL: f32 = 176.0;
    pub const SETTINGS_FOOTER: f32 = 41.0;
    pub const SELECT_W: f32 = 136.0;
    pub const SELECT_VISIBLE_ITEMS: usize = 5;
    pub const ADD_W: f32 = 560.0;
    pub const ADD_H: f32 = 350.0;
    pub const ADD_FOOTER: f32 = 48.0;
}

pub mod text {
    /// Default text carried by controls and navigation.
    pub const BASE: f32 = 13.0;
    /// Secondary copy, descriptions and state text.
    pub const BODY: f32 = 12.0;
    /// Metadata, table headings and compact labels.
    pub const META: f32 = 11.5;
    /// Monospaced values that change over time.
    pub const DATA: f32 = 11.5;
    /// Window chrome is the one deliberately compact text tier.
    pub const TITLEBAR: f32 = 10.5;
}
