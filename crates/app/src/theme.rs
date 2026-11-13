//! 配色、字体、图标、自定义控件。
//!
//! 唯一规范来源是 `docs/design/IMPLEMENT.md`：§2 的 token、§3 的字体装配、§4 的组件规格。
//! 深色是默认主题；浅色这一刀只落成常量（`light`），等第 8 刀复核浅色时再接进 `apply`。
//! 图标一律用 painter 画线，不用字形——`⚙` 这类符号在中文字体里经常缺。
#![allow(dead_code)] // token 表里有几个值要到后面几刀才用上（pal().canvas/pal().soft/pal().ink2、浅色整套）

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// 取同一个颜色的半透明版本（胶囊底、选中态用）。
pub fn tint(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

// ---------------------------------------------------------------- 调色板

/// 一套界面的颜色。
///
/// 深色是默认（`docs/design/README.md` §3 的对比度前测：同一屏里深色天然达标）；
/// 浅色是 §2.2 那套按 WCAG 反推压暗过的值，**不要「看着淡一点」再调回去**。
///
/// 为什么不写成 `const` 直接给调用方用：两套主题要在运行期切换（设置里的
/// 跟随系统/浅色/深色），而 `const` 是编译期定死的。所有读取都走 `pal()`。
#[derive(Clone, Copy)]
pub struct Palette {
    /// 窗口外/对话框背后的画布。
    pub canvas: Color32,
    /// 主区、对话框底。
    pub surface: Color32,
    /// 左栏。
    pub rail: Color32,
    /// 概览条、输入框、胶囊底、投放卡。
    pub sunken: Color32,
    /// 行/按钮悬停。
    pub hover: Color32,
    /// 选中行。
    pub sel: Color32,
    /// 表头分割、极轻底色。
    pub soft: Color32,
    /// 1px 主分割线、边框。
    pub line: Color32,
    /// 主文字、进度条填充。
    pub ink: Color32,
    /// 次级文字。
    pub ink2: Color32,
    /// 元信息、图标默认。
    pub ink3: Color32,
    /// 标签、提示。
    pub ink4: Color32,
    /// 表单说明与辅助提示；比 INK4 再淡一级，但仍满足 AA。
    pub hint: Color32,
    /// 进行中。
    pub accent: Color32,
    /// 完成/做种。
    pub ok: Color32,
    /// 暂停/等待。
    pub warn: Color32,
    /// 失败。
    pub err: Color32,
    /// 分段控件的选中格（§4 直接给的值，没进 token 表）。
    pub seg_on: Color32,
    /// 进度条轨道：深色是白 10%，浅色得反过来用墨色 10%，否则白底上看不见。
    pub track: Color32,
    /// 失败胶囊的底：深色是偏棕的 #2E1716（§4 明写别用 12% 的红），浅色是 red@8%。
    pub fail_bg: Color32,
    /// 开关关闭态的轨道色。
    pub switch_off: Color32,
    /// 对话框遮罩。用设计稿的 rgba(9,10,12,.38)：`canvas` 只比 `surface` 暗一点，
    /// 直接拿它当遮罩等于没遮（实测 21,22,26 → 19,20,23）。
    pub scrim: Color32,
}

/// 深色（默认，IMPLEMENT §2.1）。
pub const DARK: Palette = Palette {
    canvas: Color32::from_rgb(0x0F, 0x10, 0x13),
    surface: Color32::from_rgb(0x13, 0x14, 0x17),
    rail: Color32::from_rgb(0x15, 0x16, 0x1A),
    sunken: Color32::from_rgb(0x17, 0x18, 0x1C),
    hover: Color32::from_rgb(0x1B, 0x1D, 0x22),
    sel: Color32::from_rgb(0x22, 0x25, 0x2A),
    soft: Color32::from_rgb(0x1F, 0x22, 0x27),
    line: Color32::from_rgb(0x27, 0x2A, 0x30),
    ink: Color32::from_rgb(0xF2, 0xF3, 0xF5),
    ink2: Color32::from_rgb(0xB6, 0xBA, 0xC1),
    ink3: Color32::from_rgb(0x8B, 0x90, 0x9A),
    ink4: Color32::from_rgb(0x7D, 0x82, 0x8A),
    hint: Color32::from_rgb(0x85, 0x8A, 0x93),
    accent: Color32::from_rgb(0x6F, 0x9B, 0xFF),
    ok: Color32::from_rgb(0x4A, 0xD6, 0xA4),
    warn: Color32::from_rgb(0xE2, 0xA8, 0x4F),
    err: Color32::from_rgb(0xFF, 0x7D, 0x75),
    seg_on: Color32::from_rgb(0x2B, 0x2F, 0x36),
    track: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0x1A),
    fail_bg: Color32::from_rgb(0x2E, 0x17, 0x16),
    switch_off: Color32::from_rgb(0x3A, 0x3E, 0x45),
    scrim: Color32::from_rgba_unmultiplied_const(9, 10, 12, 0x61),
};

/// 浅色（可选主题，IMPLEMENT §2.2）。
pub const LIGHT: Palette = Palette {
    canvas: Color32::from_rgb(0xF1, 0xF1, 0xEF),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    rail: Color32::from_rgb(0xFA, 0xFA, 0xF8),
    sunken: Color32::from_rgb(0xF7, 0xF7, 0xF5),
    hover: Color32::from_rgb(0xF4, 0xF4, 0xF2),
    sel: Color32::from_rgb(0xF0, 0xF0, 0xEE),
    soft: Color32::from_rgb(0xF1, 0xF1, 0xEE),
    line: Color32::from_rgb(0xE7, 0xE7, 0xE4),
    ink: Color32::from_rgb(0x16, 0x17, 0x1A),
    ink2: Color32::from_rgb(0x5B, 0x5E, 0x64),
    ink3: Color32::from_rgb(0x5F, 0x63, 0x69),
    ink4: Color32::from_rgb(0x6B, 0x6E, 0x74),
    hint: Color32::from_rgb(0x6E, 0x72, 0x78),
    accent: Color32::from_rgb(0x2B, 0x5C, 0xF0),
    ok: Color32::from_rgb(0x0F, 0x76, 0x54),
    warn: Color32::from_rgb(0x8F, 0x56, 0x08),
    err: Color32::from_rgb(0xB3, 0x2F, 0x2F),
    seg_on: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    track: Color32::from_rgba_unmultiplied_const(0x16, 0x17, 0x1A, 0x14),
    fail_bg: Color32::from_rgb(0xF8, 0xE3, 0xE3),
    switch_off: Color32::from_rgb(0xD8, 0xD8, 0xD4),
    scrim: Color32::from_rgba_unmultiplied_const(0x17, 0x18, 0x1A, 0x52),
};

/// 当前调色板：0 = 深色，1 = 浅色。绘制只在 UI 线程，读一个 atomic 就够。
static ACTIVE: AtomicU8 = AtomicU8::new(0);

pub fn pal() -> &'static Palette {
    if ACTIVE.load(Ordering::Relaxed) == 1 {
        &LIGHT
    } else {
        &DARK
    }
}

pub fn set_light(light: bool) {
    ACTIVE.store(u8::from(light), Ordering::Relaxed);
}

pub fn is_light() -> bool {
    ACTIVE.load(Ordering::Relaxed) == 1
}

/// 把设置里的三档解析成「这一帧该用浅色吗」。
pub fn resolve(mode: crate::model::ThemeMode) -> bool {
    match mode {
        crate::model::ThemeMode::Light => true,
        crate::model::ThemeMode::Dark => false,
        crate::model::ThemeMode::System => system_prefers_light().unwrap_or(false),
    }
}

/// 系统是不是「浅色应用模式」：读 HKCU 的 `AppsUseLightTheme`（DWORD，1 = 浅色）。
///
/// 只是一次注册表读，不建进程——`reg.exe` 那种做法要 20–40 ms，会吃掉冷启动预算。
/// 读不到（键不存在、被策略删掉）就返回 `None`，调用方按深色处理。
pub fn system_prefers_light() -> Option<bool> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW,
        };

        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        let sub = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let value = wide("AppsUseLightTheme");
        let mut data: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        let mut ty: u32 = 0;
        // SAFETY: 两个字符串都以 0 结尾，`data`/`len`/`ty` 是本次调用独占的可写局部变量。
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                sub.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                &mut ty,
                (&raw mut data).cast(),
                &mut len,
            )
        };
        if rc != 0 || len != 4 {
            return None;
        }
        Some(data == 1)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// ---------------------------------------------------------------- 字号与尺寸

/// 行文件名（§2.3：13.5）。
pub const F_NAME: f32 = 13.5;
/// 行元信息（§2.3：12）。
pub const F_META: f32 = 12.0;

const F_BODY: f32 = 13.5;
/// 页头标题（§2.3：24 Regular，别用 Bold——雅黑粗体 24 会抢戏）。
pub const F_HEAD: f32 = 24.0;
/// 页头副标题（§2.3：12.5）。
pub const F_SUB: f32 = 12.5;
/// 概览条数值（§4：15）。
pub const F_STAT: f32 = 15.0;
/// 页脚（§2.3：11.5）。
pub const F_FOOT: f32 = 11.5;
/// 概览条标签 / 分区小标签（§2.3：11）。
pub const F_LABEL: f32 = 11.0;
pub const F_KV: f32 = 12.5;
pub const F_PILL: f32 = 11.5;

/// 内容按钮统一高度。
pub const BTN_H: f32 = 32.0;
/// 标准按钮宽度。
pub const BTN_W: f32 = 88.0;
/// 宽按钮宽度。
pub const BTN_W_WIDE: f32 = 128.0;
/// 方形图标按钮边长。
pub const BTN_ICON: f32 = 32.0;
/// 开关尺寸。
pub const SWITCH_W: f32 = 30.0;
pub const SWITCH_H: f32 = 18.0;

// §2.4 的尺寸：本刀用到的先落地，其余随对应的刀进场。
pub const TB_H: f32 = 44.0;
pub const RAIL_W: f32 = 232.0;
pub const RAIL_SM: f32 = 56.0;
pub const INS_W: f32 = 312.0;
pub const ITEM_H: f32 = 34.0;
pub const PAD: f32 = 24.0;
pub const BAND_H: f32 = 64.0;
/// 页头整块高度：18 上留白 + 30 标题行 + 4 + 17 副标题 + 18 + 概览条 64。
pub const HEAD_H: f32 = 151.0;
pub const FOOT_H: f32 = 32.0;

// 列表（§2.4）。详情栏宽度还是旧的，第 4 刀换 INS_W 312。
pub const ROW_H: f32 = 66.0;
/// 表头（§4：名称/进度/大小/状态）。
pub const THEAD_H: f32 = 26.0;
/// 行首图标块 36×36（§4）。
pub const TILE: f32 = 36.0;
/// 状态胶囊高 22（§2.4）。
pub const PILL_H: f32 = 22.0;
/// 设置对话框宽（§6：600，窗口窄时收到 min(600, 宽-64)）。
pub const DIALOG_W: f32 = 600.0;

// ---------------------------------------------------------------- 字体

/// 装字体：`Proportional = [正文西文, 中文]`，`Name("sb") = [半粗西文, 中文]`。
///
/// 西文必须排在中文前面，否则「MB/s」这类混排会被雅黑接管，字宽和正文不一致（§3）。
/// 中文只装雅黑：粗体那套要多 16.3MB 工作集，不值（§1.4）。
pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(definitions());
}

/// 拆出来是为了能不建 `Context` 就断言家族顺序（见 `tests::latin_comes_first`）。
fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let ui = first_font(&mut fonts, "ui", &["segoeui.ttf"]);
    let sb = first_font(&mut fonts, "sb", &["seguisb.ttf"]);
    let cjk = cjk_font(&mut fonts, "cjk");

    if ui || cjk {
        let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
        if ui {
            proportional.insert(0, "ui".to_owned());
        }
        if cjk {
            if ui {
                proportional.insert(1, "cjk".to_owned());
            } else {
                proportional.push("cjk".to_owned());
            }
        }
    }

    let mut sb_family: Vec<String> = Vec::new();
    if sb {
        sb_family.push("sb".to_owned());
    } else if ui {
        // 没有 seguisb 就退到正文字体，至少字宽和不换行是对的。
        sb_family.push("ui".to_owned());
    }
    if cjk {
        sb_family.push("cjk".to_owned());
    }
    if !sb_family.is_empty() {
        fonts
            .families
            .insert(FontFamily::Name("sb".into()), sb_family);
    }

    if cjk {
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push("cjk".to_owned());
    }

    fonts
}

/// 中文字族：优先用 `tools/subset-font.py` 生成的去 hinting 子集（7.9MB），
/// 没有就退回系统雅黑整份（18.8MB）。两者画出来一模一样，差别只有常驻内存。
fn cjk_font(fonts: &mut FontDefinitions, key: &str) -> bool {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("VOLANT_CJK") {
        paths.push(p.into());
    }
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        paths.push(
            std::path::PathBuf::from(dir)
                .join("volant")
                .join("cjk-subset.ttf"),
        );
    }
    for path in paths {
        if let Ok(bytes) = std::fs::read(&path) {
            fonts
                .font_data
                .insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
            return true;
        }
    }
    first_font(fonts, key, &["msyh.ttc", "msyhl.ttc", "simhei.ttf"])
}

/// 从 `C:\Windows\Fonts` 里读第一个存在的文件，读到了就塞进 `key`。缺文件不算错，只是没有这套字。
fn first_font(fonts: &mut FontDefinitions, key: &str, files: &[&str]) -> bool {
    for file in files {
        let path = format!(r"C:\Windows\Fonts\{file}");
        if let Ok(bytes) = std::fs::read(&path) {
            fonts
                .font_data
                .insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
            return true;
        }
    }
    false
}

/// 半粗那一路的 `FontId`。行文件名、标题会用到（本刀还没接进 ui.rs）。
pub fn font_sb(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("sb".into()))
}

// ---------------------------------------------------------------- 样式

/// 当前配色对应的 egui / Windows 窗口主题。
pub fn window_theme() -> egui::Theme {
    if is_light() {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    }
}

/// 当前 `Visuals` + 字号，并同步 Windows 无边框窗口的系统主题。
pub fn apply(ctx: &egui::Context) {
    let window_theme = window_theme();
    let mut v = if is_light() {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    v.panel_fill = pal().surface;
    v.window_fill = pal().surface;
    v.extreme_bg_color = pal().sunken;
    v.faint_bg_color = pal().soft;
    v.override_text_color = Some(pal().ink);
    v.selection.bg_fill = tint(pal().accent, 0x24); // pal().accent @ 14%
    v.selection.stroke = Stroke::new(1.0, pal().accent);
    v.hyperlink_color = pal().accent;
    v.window_stroke = Stroke::new(1.0, pal().line);
    v.window_corner_radius = CornerRadius::same(14);

    // 控件底按 §3 一律透明：按钮、开关、分段控件都要自绘（§4），只剩悬停/按下的状态色。
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_fill = Color32::TRANSPARENT;
        w.bg_stroke = Stroke::NONE;
        w.corner_radius = CornerRadius::same(9); // r7
        w.expansion = 0.0;
    }
    // 例外：egui::Button 的底色来自 weak_bg_fill，这里先不动（设置面板还全是它），
    // 第 7 刀把对话框改成自绘按钮时连同这三行一起删。
    v.widgets.inactive.weak_bg_fill = pal().soft;
    v.widgets.hovered.weak_bg_fill = pal().sel;
    v.widgets.active.weak_bg_fill = pal().seg_on;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal().ink2);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, pal().ink3);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, pal().ink2);
    v.widgets.active.fg_stroke = Stroke::new(1.0, pal().ink);

    let mut style = (*ctx.style_of(window_theme)).clone();
    style.visuals = v;
    style.text_styles = [
        (egui::TextStyle::Heading, FontId::proportional(F_HEAD)),
        (egui::TextStyle::Body, FontId::proportional(F_BODY)),
        (egui::TextStyle::Monospace, FontId::monospace(12.5)),
        (egui::TextStyle::Button, FontId::proportional(12.5)),
        (egui::TextStyle::Small, FontId::proportional(F_PILL)),
    ]
    .into();
    // 间距、按钮内边距、控件高度都还是旧值：这些是布局，跟着第 2–7 刀一起换，
    // 这一刀只动颜色与字体，免得旧界面在中间态被挤歪。
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating = true;
    style.spacing.scroll.foreground_color = true;
    style.spacing.scroll.dormant_handle_opacity = 0.72;
    style.spacing.scroll.active_handle_opacity = 0.88;
    style.spacing.scroll.interact_handle_opacity = 1.0;
    style.spacing.scroll.dormant_background_opacity = 0.0;
    style.spacing.scroll.active_background_opacity = 0.0;
    style.spacing.scroll.interact_background_opacity = 0.0;
    style.spacing.window_margin = egui::Margin::same(0);
    ctx.set_style_of(window_theme, style);
    // egui 会把这里同步给 winit。浅色 UI 若继续使用 `Theme::Dark`，Windows
    // 会给无边框窗口配深色 DWM 边框，在浅色背景上看起来像一圈很重的黑影。
    ctx.set_theme(window_theme);
}

// ---------------------------------------------------------------- 文字

/// 单行文字，超长用省略号截断。
pub fn galley(ui: &Ui, text: &str, size: f32, color: Color32, max_width: f32) -> Arc<egui::Galley> {
    galley_font(ui, FontId::proportional(size), text, color, max_width)
}

/// 半粗单行。第 3 刀的行文件名要用，本刀先备着。
pub fn galley_sb(
    ui: &Ui,
    text: &str,
    size: f32,
    color: Color32,
    max_width: f32,
) -> Arc<egui::Galley> {
    galley_font(ui, font_sb(size), text, color, max_width)
}

fn galley_font(
    ui: &Ui,
    font: FontId,
    text: &str,
    color: Color32,
    max_width: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.painter().layout_job(job)
}

/// 多行省略：最多 `max_rows` 行，超出接 `…`（详情栏文件名、路径用）。
pub fn galley_wrap(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
    max_rows: usize,
) -> Arc<egui::Galley> {
    galley_wrap_with(text, font, color, max_width, max_rows, true, ui)
}

/// 文件名优先在连字符、下划线、空格和扩展名前换行。
pub fn galley_filename(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
    max_rows: usize,
) -> Arc<egui::Galley> {
    let display = filename_wrap_text(text);
    galley_wrap_with(&display, font, color, max_width, max_rows, false, ui)
}

fn galley_wrap_with(
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
    max_rows: usize,
    break_anywhere: bool,
    ui: &Ui,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width;
    job.wrap.max_rows = max_rows;
    job.wrap.break_anywhere = break_anywhere;
    job.wrap.overflow_character = Some('…');
    ui.painter().layout_job(job)
}

fn filename_wrap_text(text: &str) -> String {
    let extension_dot = text.rfind('.').filter(|index| *index > 0);
    let mut out = String::with_capacity(text.len() + 8);
    for (index, ch) in text.char_indices() {
        if extension_dot == Some(index) {
            out.push('\u{200B}');
        }
        out.push(ch);
        if matches!(ch, '-' | '_' | '/' | '\\' | ' ') {
            out.push('\u{200B}');
        }
    }
    out
}

/// 30×18 开关：开启态用 INK、关闭态用主题灰，滑块按深浅主题反转，120ms。
/// 位置由调用方给定（左栏是在自绘的矩形上贴一个开关）。
pub fn switch_at(ui: &mut Ui, rect: Rect, on: bool, id: &str, label: &str) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let enabled = ui.is_enabled();
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("sw"), on, 0.12);
    let p = ui.painter();
    let off = if enabled {
        pal().switch_off
    } else {
        pal().switch_off.gamma_multiply(0.55)
    };
    p.rect_filled(rect, CornerRadius::same(9), mix(off, pal().ink, t));
    let x = rect.left() + 2.0 + t * (rect.width() - rect.height());
    p.circle_filled(
        pos2(x + 7.0, rect.center().y),
        7.0,
        mix(pal().ink, pal().canvas, t),
    );
    if resp.hovered() && enabled {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.has_focus() {
        p.rect_stroke(
            rect.expand(2.5),
            CornerRadius::same(11),
            Stroke::new(1.5, pal().ink2),
            StrokeKind::Inside,
        );
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, on, label));
    resp.on_hover_text(label)
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

// ---------------------------------------------------------------- 图标

/// 图标显示区域；绘制几何使用 24px 网格，再等比缩放到 16px。
pub const ICON: f32 = 16.0;
const ICON_GRID: f32 = 24.0;
const ICON_STROKE: f32 = 1.5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Plus,
    Gear,
    Minimize,
    Maximize,
    Close,
    Pause,
    Play,
    Trash,
    /// 下载中：下箭头 + 托盘。
    Down,
    /// 完成/做种：对勾。
    Check,
    /// 暂停：一张纸。
    Doc,
    /// 全部下载：有层级的多行队列。
    Queue,
    /// 重新下载：循环箭头。
    Refresh,
    Alert,
    /// 文件夹（设置里「打开下载目录」）。
    Folder,
    /// 剪贴板/链接。
    Link,
    /// 任务状态：圆框内向下箭头。
    TaskDownload,
    /// 任务状态：圆框内两条竖线。
    TaskPaused,
}

pub fn paint_icon(p: &egui::Painter, center: Pos2, color: Color32, icon: Icon) {
    let s = Stroke::new(ICON_STROKE, color);
    let scale = ICON / ICON_GRID;
    // 24px 网格坐标（左上 0,0）→ 16px 显示坐标。
    let at =
        |x: f32, y: f32| center + (vec2(x, y) - vec2(ICON_GRID / 2.0, ICON_GRID / 2.0)) * scale;
    let line = |a: (f32, f32), b: (f32, f32)| {
        let points = [at(a.0, a.1), at(b.0, b.1)];
        p.line_segment(points, s);
        for point in points {
            p.circle_filled(point, ICON_STROKE / 2.0, color);
        }
    };
    let polyline = |points: &[(f32, f32)], closed: bool| {
        let points = points.iter().map(|(x, y)| at(*x, *y)).collect::<Vec<_>>();
        let mut shape = egui::Shape::line(points.clone(), s);
        if let egui::Shape::Path(path) = &mut shape {
            path.closed = closed;
        }
        p.add(shape);
        for point in points {
            p.circle_filled(point, ICON_STROKE / 2.0, color);
        }
    };
    let arc = |center: (f32, f32), radius: f32, from: f32, to: f32| {
        let points = (0..=12)
            .map(|step| {
                let angle = from + (to - from) * step as f32 / 12.0;
                let (sin, cos) = angle.sin_cos();
                (center.0 + cos * radius, center.1 + sin * radius)
            })
            .collect::<Vec<_>>();
        polyline(&points, false);
        points
    };

    match icon {
        Icon::Plus => {
            line((12.0, 4.5), (12.0, 19.5));
            line((4.5, 12.0), (19.5, 12.0));
        }
        Icon::Gear => {
            p.circle_stroke(at(12.0, 12.0), 3.1 * scale, Stroke::new(ICON_STROKE, color));
            p.circle_stroke(at(12.0, 12.0), 7.0 * scale, s);
            for i in 0..8 {
                let angle = std::f32::consts::TAU * i as f32 / 8.0;
                let (sin, cos) = angle.sin_cos();
                line(
                    (12.0 + cos * 7.0, 12.0 + sin * 7.0),
                    (12.0 + cos * 8.2, 12.0 + sin * 8.2),
                );
            }
        }
        Icon::Minimize => line((5.0, 12.0), (19.0, 12.0)),
        Icon::Maximize => {
            p.rect_stroke(
                Rect::from_min_max(at(5.0, 5.0), at(19.0, 19.0)),
                CornerRadius::same(1),
                s,
                StrokeKind::Inside,
            );
        }
        Icon::Close => {
            line((5.5, 5.5), (18.5, 18.5));
            line((18.5, 5.5), (5.5, 18.5));
        }
        Icon::Pause => {
            line((8.5, 5.5), (8.5, 18.5));
            line((15.5, 5.5), (15.5, 18.5));
        }
        Icon::Play => polyline(&[(8.0, 5.2), (18.0, 12.0), (8.0, 18.8)], true),
        Icon::Trash => {
            line((6.0, 7.0), (18.0, 7.0));
            polyline(&[(9.0, 7.0), (9.0, 4.8), (15.0, 4.8), (15.0, 7.0)], false);
            polyline(&[(7.3, 7.0), (8.1, 19.0), (15.9, 19.0), (16.7, 7.0)], false);
            line((10.0, 10.5), (10.0, 15.5));
            line((14.0, 10.5), (14.0, 15.5));
        }
        Icon::Down => {
            line((12.0, 4.0), (12.0, 14.2));
            line((7.8, 10.2), (12.0, 14.5));
            line((16.2, 10.2), (12.0, 14.5));
            polyline(
                &[(5.0, 16.2), (5.0, 19.5), (19.0, 19.5), (19.0, 16.2)],
                false,
            );
        }
        Icon::Check => {
            polyline(&[(5.0, 12.6), (9.1, 16.8), (19.0, 6.6)], false);
        }
        Icon::Doc => {
            polyline(
                &[
                    (6.0, 3.5),
                    (13.8, 3.5),
                    (18.0, 7.7),
                    (18.0, 20.5),
                    (6.0, 20.5),
                ],
                true,
            );
            polyline(&[(13.8, 3.5), (13.8, 7.7), (18.0, 7.7)], false);
            line((9.0, 11.2), (15.0, 11.2));
            line((9.0, 14.3), (15.0, 14.3));
            line((9.0, 17.4), (13.5, 17.4));
        }
        Icon::Queue => {
            for y in [6.2, 12.0, 17.8] {
                line((4.5, y), (7.5, y));
                line((10.0, y), (19.5, y));
            }
        }
        Icon::Refresh => {
            let top = arc((12.0, 12.0), 6.6, 3.55, 5.75);
            let bottom = arc((12.0, 12.0), 6.6, 0.42, 2.62);
            if let Some(end) = top.last() {
                line((end.0 - 2.7, end.1 + 1.1), *end);
            }
            if let Some(end) = bottom.last() {
                line((end.0 + 2.7, end.1 - 1.1), *end);
            }
        }
        Icon::Alert => {
            p.circle_stroke(at(12.0, 12.0), 8.2 * scale, s);
            line((12.0, 7.3), (12.0, 12.9));
            p.circle_filled(at(12.0, 16.2), ICON_STROKE / 2.0, color);
        }
        Icon::Folder => {
            p.rect_stroke(
                Rect::from_min_max(at(3.8, 6.8), at(20.2, 19.2)),
                CornerRadius::same(2),
                s,
                StrokeKind::Inside,
            );
            polyline(
                &[(3.8, 6.8), (5.8, 4.8), (9.8, 4.8), (11.8, 6.8), (20.2, 6.8)],
                false,
            );
        }
        Icon::Link => {
            p.circle_stroke(at(8.2, 15.3), 3.2 * scale, s);
            p.circle_stroke(at(15.8, 8.7), 3.2 * scale, s);
            line((10.5, 13.0), (13.5, 11.0));
        }
        Icon::TaskDownload => {
            p.circle_stroke(at(12.0, 12.0), 8.3 * scale, s);
            line((12.0, 7.6), (12.0, 15.6));
            line((9.0, 12.7), (12.0, 15.7));
            line((15.0, 12.7), (12.0, 15.7));
        }
        Icon::TaskPaused => {
            p.circle_stroke(at(12.0, 12.0), 8.3 * scale, s);
            line((9.8, 8.4), (9.8, 15.6));
            line((14.2, 8.4), (14.2, 15.6));
        }
    }
}

// ---------------------------------------------------------------- 自绘控件

/// 分段选择器：pal().sunken 底、r7、2px 内边距，选中格 `pal().seg_on` + pal().ink（§4）。
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, current: T, options: &[(T, &str)]) -> Option<T> {
    if options.is_empty() {
        return None;
    }

    let font = FontId::proportional(F_PILL);
    let pad = 2.0;
    // 整行被 `ui.set_enabled(false)` 关掉时（设置里做种关掉后的分享率）：
    // 常显但不可点，文字降到 pal().ink4@50%（§6）。
    let enabled = ui.is_enabled();
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, label)| {
            let w = ui
                .painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), pal().ink3)
                .size()
                .x;
            (w + 20.0).max(34.0)
        })
        .collect();
    let total = widths.iter().sum::<f32>() + pad * 2.0;

    let (rect, _) = ui.allocate_exact_size(vec2(total, 28.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(9), pal().sunken);

    let base_id = ui.next_auto_id();
    let mut picked = None;
    let mut x = rect.left() + pad;
    for (i, (value, label)) in options.iter().enumerate() {
        let cell =
            Rect::from_min_size(pos2(x, rect.top() + pad), vec2(widths[i], 28.0 - pad * 2.0));
        x += widths[i];

        let on = *value == current;
        let resp = ui.interact(cell, base_id.with(i), Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, (*label).to_owned())
        });

        let p = ui.painter();
        if on {
            p.rect_filled(cell, CornerRadius::same(7), pal().seg_on);
            // 浅色的选中格是白底压在 #F7F7F5 轨道上，只靠填充几乎看不出来（对比 1.04:1），
            // 补一道 hairline 边框；深色那格本来就差着 8% 亮度，加不加都一样。
            if is_light() {
                p.rect_stroke(
                    cell,
                    CornerRadius::same(7),
                    Stroke::new(1.0, pal().line),
                    StrokeKind::Inside,
                );
            }
        } else if enabled && resp.hovered() {
            p.rect_filled(cell, CornerRadius::same(7), pal().hover);
        }
        let fg = if !enabled {
            pal().ink4.gamma_multiply(0.5)
        } else if on {
            pal().ink
        } else {
            pal().ink3
        };
        p.text(
            cell.center(),
            Align2::CENTER_CENTER,
            *label,
            font.clone(),
            fg,
        );
        if resp.has_focus() && enabled {
            p.rect_stroke(
                cell,
                CornerRadius::same(7),
                Stroke::new(1.0, pal().accent),
                StrokeKind::Inside,
            );
        }
        if resp.clicked() && !on && enabled {
            picked = Some(*value);
        }
    }
    picked
}

/// 设置里的一行：标题、说明、控件。
///
/// §6 要的是左标签右控件的两栏，但调用点里有 `desired_width(INFINITY)` 的输入框，
/// 也有一行塞两个 segmented（做种），右对齐会互相挤；等第 7 刀连 ui.rs 一起改。
pub fn field(ui: &mut Ui, title: &str, hint: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).size(F_KV).color(pal().ink));
    if !hint.is_empty() {
        ui.label(RichText::new(hint).size(F_LABEL).color(pal().ink4));
    }
    ui.add_space(6.0);
    add(ui);
}

pub fn divider(ui: &mut Ui, pad: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, pal().line),
    );
    ui.add_space(pad);
}

/// 行内进度条：轨道 `#FFFFFF1A`，填充按状态色（§4）。
pub fn progress_bar(p: &egui::Painter, rect: Rect, frac: f32, color: Color32) {
    let r = CornerRadius::same(3);
    p.rect_filled(rect, r, pal().track);
    let w = rect.width() * frac.clamp(0.0, 1.0);
    if w > 0.5 {
        p.rect_filled(
            Rect::from_min_size(rect.min, vec2(w, rect.height())),
            r,
            color,
        );
    }
}

/// 行首色块里的图标，按状态选（§4）。
pub fn icon_for(status: crate::model::Status) -> Icon {
    use crate::model::Status;
    match status {
        Status::Active | Status::Queued | Status::Probing => Icon::TaskDownload,
        Status::Seeding | Status::Done => Icon::Check,
        Status::Failed => Icon::Close,
        Status::Paused => Icon::TaskPaused,
    }
}

/// 状态色：下载中蓝、做种/完成绿、失败红，排队与暂停保持中性（§2.1 的用途说明）。
pub fn status_color(status: crate::model::Status) -> Color32 {
    use crate::model::Status;
    match status {
        Status::Queued => pal().ink3,
        Status::Probing => pal().warn,
        Status::Active => pal().accent,
        Status::Seeding => pal().ok,
        Status::Paused => pal().ink3,
        Status::Done => pal().ok,
        Status::Failed => pal().err,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: Color32, b: Color32) -> f32 {
        fn channel(value: u8) -> f32 {
            let v = value as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(c: Color32) -> f32 {
            0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// 拉丁排在中文前面、`sb` 家族带同一个中文回退——这两条错了截图上看不出来，只会觉得"字有点不对劲"。
    #[test]
    fn latin_comes_first() {
        let fonts = definitions();
        let prop = &fonts.families[&FontFamily::Proportional];
        let (Some(latin), Some(cjk)) = (
            prop.iter().position(|name| name == "ui"),
            prop.iter().position(|name| name == "cjk"),
        ) else {
            panic!("这台机器缺字体文件，Proportional = {prop:?}");
        };
        assert!(latin < cjk, "Proportional 里拉丁必须在中文前面：{prop:?}");

        let sb = &fonts.families[&FontFamily::Name("sb".into())];
        assert!(
            sb.iter().any(|name| name == "sb" || name == "ui"),
            "sb 家族没有半粗西文：{sb:?}"
        );
        assert!(
            sb.iter().any(|name| name == "cjk"),
            "sb 家族缺中文回退：{sb:?}"
        );
    }

    #[test]
    fn task_statuses_use_dedicated_download_and_pause_icons() {
        use crate::model::Status;
        for status in [Status::Queued, Status::Probing, Status::Active] {
            assert!(matches!(icon_for(status), Icon::TaskDownload));
        }
        assert!(matches!(icon_for(Status::Paused), Icon::TaskPaused));
        assert_ne!(icon_for(Status::Paused), Icon::Pause);
    }

    /// 主题开关只有全局一个 `ACTIVE`，所以调色板相关的断言都放在同一个测试里，避免并行串台。
    #[test]
    fn palette_follows_the_switch() {
        set_light(true);
        assert!(is_light());
        assert_eq!(pal().canvas, LIGHT.canvas);
        assert_eq!(window_theme(), egui::Theme::Light);

        set_light(false);
        assert!(!is_light());
        assert_eq!(pal().surface, DARK.surface);
        assert_eq!(window_theme(), egui::Theme::Dark);

        // 两套主题必须是两套值：搬 token 时最容易犯的错就是浅色漏掉一项，
        // 而截图上看不出来（白底上的白色）。
        for (a, b) in [
            (DARK.canvas, LIGHT.canvas),
            (DARK.surface, LIGHT.surface),
            (DARK.ink, LIGHT.ink),
            (DARK.line, LIGHT.line),
            (DARK.seg_on, LIGHT.seg_on),
            (DARK.track, LIGHT.track),
        ] {
            assert_ne!(a, b);
        }

        use crate::model::ThemeMode;
        assert!(resolve(ThemeMode::Light));
        assert!(!resolve(ThemeMode::Dark));
        assert_eq!(
            resolve(ThemeMode::System),
            system_prefers_light().unwrap_or(false)
        );
    }

    #[test]
    fn light_auxiliary_and_failure_text_meet_wcag_aa() {
        assert!(contrast(LIGHT.ink3, LIGHT.surface) >= 4.5);
        assert!(contrast(LIGHT.ink4, LIGHT.surface) >= 4.5);
        assert!(contrast(LIGHT.ink3, LIGHT.sunken) >= 4.5);
        assert!(contrast(LIGHT.hint, LIGHT.surface) >= 4.5);
        assert!(contrast(LIGHT.hint, LIGHT.sunken) >= 4.5);
        assert!(contrast(LIGHT.err, LIGHT.fail_bg) >= 4.5);
        assert!(contrast(DARK.ink4, DARK.surface) >= 4.5);
        assert!(contrast(DARK.hint, DARK.surface) >= 4.5);
    }

    #[test]
    fn filenames_offer_semantic_wrap_points() {
        let wrapped = filename_wrap_text("ubuntu-24.04.2-desktop-amd64.iso");
        assert!(wrapped.contains('\u{200B}'));
        assert!(wrapped.contains("\u{200B}.iso"));
        assert_eq!(
            wrapped.replace('\u{200B}', ""),
            "ubuntu-24.04.2-desktop-amd64.iso"
        );
    }
}
