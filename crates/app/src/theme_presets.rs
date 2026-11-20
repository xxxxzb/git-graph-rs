//! 主题配色预设：保留 Ely 默认暖灰主题，并新增 Variant D（Studio 高对比 Zinc + Sky）配色方案。

use ely_core::theme::{Mode, Palette, Theme};
use gpui::{App, Hsla, rgb, rgba};

use crate::state::ThemePreset;

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// 构造 Variant D 浅色模式配色（极净 Zinc 灰白层级 + Sky Blue 强调色）。
pub fn studio_light_palette() -> Palette {
    let mut palette = Palette::light(false);
    palette.bg = c(0xfafafa);
    palette.surface = c(0xffffff);
    palette.sunken = c(0xf4f4f5);
    palette.overlay = c(0xffffff);
    palette.hover = c(0xf4f4f5);
    palette.active = c(0xe4e4e7);
    palette.border = c(0xe4e4e7);
    palette.border_strong = c(0xd4d4d8);
    palette.fg = c(0x09090b);
    palette.fg_muted = c(0x52525b);
    palette.fg_subtle = c(0x71717a);
    palette.fg_disabled = c(0xa1a1aa);
    palette.accent = c(0x0284c7);
    palette.accent_hover = c(0x0369a1);
    palette.on_accent = c(0xffffff);
    palette.focus = c(0x0ea5e9);
    palette.link = c(0x0284c7);
    palette.selection = rgba(0x0284c72b).into();
    palette.success = c(0x059669);
    palette.warning = c(0xd97706);
    palette.danger = c(0xe11d48);
    palette.info = c(0x0284c7);
    palette.success_subtle = c(0xecfdf5);
    palette.warning_subtle = c(0xfffbeb);
    palette.danger_subtle = c(0xfff1f2);
    palette.info_subtle = c(0xf0f9ff);
    palette.backdrop = rgba(0x09090b4d).into();
    palette.glass = rgba(0xfafafacc).into();
    palette.shadow = c(0x09090b);
    palette.tooltip_bg = c(0x18181b);
    palette.tooltip_fg = c(0xfafafa);
    palette.ink = c(0x09090b);
    palette
}

/// 构造 Variant D 深色模式配色（Obsidian Zinc 深邃底色 + 明亮 Sky-400 强调色）。
pub fn studio_dark_palette() -> Palette {
    let mut palette = Palette::dark(false);
    palette.bg = c(0x09090b);
    palette.surface = c(0x111114);
    palette.sunken = c(0x18181b);
    palette.overlay = c(0x1c1c21);
    palette.hover = c(0x202026);
    palette.active = c(0x27272a);
    palette.border = c(0x27272a);
    palette.border_strong = c(0x3f3f46);
    palette.fg = c(0xfafafa);
    palette.fg_muted = c(0xa1a1aa);
    palette.fg_subtle = c(0x71717a);
    palette.fg_disabled = c(0x52525b);
    palette.accent = c(0x38bdf8);
    palette.accent_hover = c(0x7dd3fc);
    palette.on_accent = c(0x09090b);
    palette.focus = c(0x38bdf8);
    palette.link = c(0x38bdf8);
    palette.selection = rgba(0x38bdf833).into();
    palette.success = c(0x34d399);
    palette.warning = c(0xfbbf24);
    palette.danger = c(0xfb7185);
    palette.info = c(0x38bdf8);
    palette.success_subtle = c(0x0b2922);
    palette.warning_subtle = c(0x2e2209);
    palette.danger_subtle = c(0x33121b);
    palette.info_subtle = c(0x0c2438);
    palette.backdrop = rgba(0x0000008c).into();
    palette.glass = rgba(0x111114cc).into();
    palette.shadow = c(0x000000);
    palette.tooltip_bg = c(0x27272a);
    palette.tooltip_fg = c(0xfafafa);
    palette.ink = c(0x09090b);
    palette
}

/// 将指定配色预设应用到全局 `Theme`。
pub fn apply_theme_preset(preset: ThemePreset, cx: &mut App) {
    match preset {
        ThemePreset::Default => {
            Theme::set_palette(Mode::Light, None, cx);
            Theme::set_palette(Mode::Dark, None, cx);
        }
        ThemePreset::Studio => {
            Theme::set_palette(Mode::Light, Some(studio_light_palette()), cx);
            Theme::set_palette(Mode::Dark, Some(studio_dark_palette()), cx);
        }
    }
}
