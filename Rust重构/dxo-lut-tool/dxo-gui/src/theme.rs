//! 设计令牌与控件样式 —— 嘻哈视觉规范（Hip-Hop Design System），暗色版。
//!
//! 视觉语言（对齐 `docs/UI-样张-嘻哈.html` 实际渲染）：
//!   近黑画布 + 浅色文字（卡面反色系统反过来：现在是「暗背景 / 亮文字」）+ 单一
//!   DxO 蓝绿 accent；零圆角、2px 硬边、无阴影。白色仅留给极个别"亮卡"（当前无，
//!   保留 `panel`/`card` 以备）。其它模块只允许引用本模块，不得出现颜色 / 间距字面量。
//!
//! 源规范：`docs/UI-设计规范.md`；落地参照 `docs/UI-样张-嘻哈.html`。

use iced::widget::{button, container, scrollable, slider, text_input};
use iced::{Background, Border, Color, Shadow, Theme};

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

const fn hex(v: u32) -> Color {
    rgba((v >> 16) as u8, (v >> 8) as u8, v as u8, 1.0)
}

// ---------------------------------------------------------------- 色彩

pub mod color {
    use super::*;

    // 画布（近黑街头底色）
    pub const CANVAS_BASE: Color = hex(0x161618);
    // 画布上的文字（浅色 —— 状态栏 / 暗预览帧标题）
    pub const CANVAS_TEXT: Color = hex(0xEDEDEF);
    pub const CANVAS_TEXT_DIM: Color = hex(0x8A8A92);

    // 暗色"抬升"表面（输入框、hover、按压等，比画布略亮，制造层次）
    pub const SURFACE: Color = hex(0x161618); // 大区与画布同色（平铺，不另起白卡）
    pub const SURFACE_RAISED: Color = hex(0x202024);
    pub const SURFACE_FIELD: Color = hex(0x232328); // 输入框填充（略亮以显可编辑）
    pub const SURFACE_HOVER: Color = hex(0x2A2A30);
    pub const SURFACE_PRESSED: Color = hex(0x34343C);
    pub const SURFACE_MEDIA: Color = hex(0x0E0E10); // 暗预览帧（卡内的内容层，最暗）

    // 文字（亮色 —— 暗背景上的反色系统）
    pub const LABEL_PRIMARY: Color = hex(0xEDEDEF); // 主文字 ≈ 近白
    pub const LABEL_SECONDARY: Color = hex(0xB8B8BE); // 次级
    pub const LABEL_TERTIARY: Color = hex(0x8A8A92); // 辅助 / 三级
    pub const LABEL_QUATERNARY: Color = hex(0x6A6A6C); // 最弱

    // 描边：暗色模式下用"比画布略亮的灰"做发丝边；强描边略亮
    pub const BORDER_STRONG: Color = hex(0x4A4A52);
    pub const BORDER_HAIRLINE: Color = hex(0x33333A);
    pub const BORDER_DARKMEDIA: Color = hex(0x4A4A52); // 暗预览帧上的浅边（需可见）

    // 强调：DxO 蓝绿
    pub const ACCENT: Color = hex(0x4EADB9);
    pub const ACCENT_HOVER: Color = hex(0x5FBEC9);
    pub const ACCENT_PRESSED: Color = hex(0x3E9AA6);
    pub const ACCENT_INK: Color = hex(0x10221F); // accent 上的文字（深色）
    pub const ACCENT_SOFT: Color = rgba(78, 173, 185, 0.16);

    // 语义色（刻意不与蓝绿同相，维持区分）
    pub const SUCCESS: Color = hex(0x1F9D55);
    pub const SUCCESS_SOFT: Color = rgba(31, 157, 85, 0.16);
    pub const WARNING: Color = hex(0xB5780A);
    pub const WARNING_SOFT: Color = rgba(181, 120, 10, 0.16);
    pub const GOLD: Color = hex(0x9A6B00);
    pub const GOLD_SOFT: Color = rgba(154, 107, 0, 0.16);
    pub const WARNING_DIM: Color = hex(0xC2362B);
    pub const DANGER: Color = hex(0xC2362B);
    pub const DANGER_SOFT: Color = rgba(194, 54, 43, 0.14);
}

// ---------------------------------------------------------------- 间距 / 圆角 / 尺寸

pub mod space {
    pub const S1: f32 = 2.0;
    pub const S2: f32 = 4.0;
    pub const S3: f32 = 8.0;
    pub const S4: f32 = 12.0;
    pub const S5: f32 = 16.0;
    pub const S6: f32 = 20.0;
    pub const S7: f32 = 24.0;
    pub const S8: f32 = 32.0;
}

/// 全局零圆角（硬边方框）。
pub mod radius {
    pub const XS: f32 = 0.0;
    pub const SM: f32 = 0.0;
    pub const MD: f32 = 0.0;
    pub const LG: f32 = 0.0;
    pub const XL: f32 = 0.0;
    pub const PILL: f32 = 0.0;
}

pub mod size {
    pub const TITLEBAR_H: f32 = 52.0;
    pub const STATUSBAR_H: f32 = 34.0;
    pub const SIDEBAR_W: f32 = 256.0;
    pub const CONTENT_PAD: f32 = 20.0;
    pub const ROW_H: f32 = 52.0;
    /// 库列表最多占的高度（超出滚动），正好 4 行
    pub const LIB_MAX_H: f32 = 224.0;
    pub const CONTROL_SM: f32 = 28.0;
    pub const CONTROL_MD: f32 = 32.0;
    pub const CONTROL_LG: f32 = 36.0;
    pub const LABEL_COL: f32 = 56.0;
    pub const NUM_COL: f32 = 44.0;
}

pub mod motion {
    pub const INSTANT_MS: u64 = 100;
    pub const FAST_MS: u64 = 160;
}

// ---------------------------------------------------------------- 工具函数

pub fn bd(color: Color, width: f32, r: f32) -> Border {
    let mut b = Border::default().rounded(r);
    b.color = color;
    b.width = width;
    b
}

fn solid(c: Color) -> Option<Background> {
    Some(Background::Color(c))
}

// ---------------------------------------------------------------- 容器样式

/// 大区（侧栏 / 内容）：平铺暗画布，无背景填充、无边框，与外层画布融为一体。
pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::CANVAS_TEXT),
        background: Some(Background::Color(color::CANVAS_BASE)),
        border: Border::default(),
        shadow: Shadow::default(),
    }
}

/// 侧栏：平铺暗画布 + 右侧发丝分隔（对齐样张菜单栏右边框）。
pub fn sidebar(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::CANVAS_TEXT),
        background: Some(Background::Color(color::CANVAS_BASE)),
        border: bd(color::BORDER_HAIRLINE, 2.0, radius::XL),
        shadow: Shadow::default(),
    }
}

/// 卡片 / 列表行底：暗表面 + 发丝边（不再用白卡）。
pub fn card(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::LABEL_PRIMARY),
        background: Some(Background::Color(color::SURFACE_RAISED)),
        border: bd(color::BORDER_HAIRLINE, 2.0, radius::LG),
        shadow: Shadow::default(),
    }
}

/// 预览帧：内容层外框，最暗底（让图像跳出来），浅边可见。
pub fn frame(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::CANVAS_TEXT_DIM),
        background: Some(Background::Color(color::SURFACE_MEDIA)),
        border: bd(color::BORDER_DARKMEDIA, 2.0, radius::MD),
        shadow: Shadow::default(),
    }
}

/// 预览画布：最暗实色内容层。
pub fn media(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::CANVAS_TEXT_DIM),
        background: Some(Background::Color(color::SURFACE_MEDIA)),
        border: bd(color::BORDER_DARKMEDIA, 2.0, radius::MD),
        shadow: Shadow::default(),
    }
}

/// 1px 分隔线容器（配合 height(1) 使用）：暗色发丝线。
pub fn separator(_: &Theme) -> container::Style {
    container::Style {
        text_color: None,
        background: Some(Background::Color(color::BORDER_HAIRLINE)),
        border: Border::default(),
        shadow: Shadow::default(),
    }
}

/// 小徽章 / 序号方块：透明底、浅字、发丝边。
pub fn chip(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::LABEL_TERTIARY),
        background: None,
        border: bd(color::BORDER_HAIRLINE, 2.0, radius::XS),
        shadow: Shadow::default(),
    }
}

/// 分段控件外壳：透明底、发丝边。
pub fn segmented_shell(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::LABEL_SECONDARY),
        background: None,
        border: bd(color::BORDER_HAIRLINE, 2.0, radius::MD),
        shadow: Shadow::default(),
    }
}

/// 滑块圆点（开关用）：浅方块（落在暗轨道上才看得清）。
pub fn knob(_: &Theme) -> container::Style {
    container::Style {
        text_color: None,
        background: Some(Background::Color(color::CANVAS_TEXT)),
        border: Border::default().rounded(radius::PILL),
        shadow: Shadow::default(),
    }
}

/// 状态胶囊：透明底、浅字、发丝边。
pub fn pill(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::LABEL_SECONDARY),
        background: None,
        border: bd(color::BORDER_HAIRLINE, 2.0, radius::PILL),
        shadow: Shadow::default(),
    }
}

/// 品牌标（已弃用：标题栏移除后不再调用；保留签名）。
pub fn logo(_: &Theme) -> container::Style {
    container::Style {
        text_color: None,
        background: Some(Background::Color(color::ACCENT_SOFT)),
        border: bd(color::ACCENT, 2.0, radius::PILL),
        shadow: Shadow::default(),
    }
}

/// 选中行左侧强调竖条。
pub fn accent_bar(_: &Theme) -> container::Style {
    container::Style {
        text_color: None,
        background: Some(Background::Color(color::ACCENT)),
        border: Border::default().rounded(0.0),
        shadow: Shadow::default(),
    }
}

/// 完全透明的占位容器（保持布局对齐）。
pub fn invisible(_: &Theme) -> container::Style {
    container::Style {
        text_color: None,
        background: None,
        border: Border::default(),
        shadow: Shadow::default(),
    }
}

// ---------------------------------------------------------------- 按钮

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

pub fn button_style(kind: Kind) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        use button::Status;

        if matches!(status, Status::Disabled) {
            return button::Style {
                background: solid(match kind {
                    Kind::Primary => rgba(78, 173, 185, 0.30),
                    Kind::Secondary => color::SURFACE_HOVER,
                    Kind::Ghost => Color::TRANSPARENT,
                    Kind::Danger => Color::TRANSPARENT,
                }),
                text_color: color::LABEL_QUATERNARY,
                border: match kind {
                    Kind::Secondary => bd(color::BORDER_HAIRLINE, 2.0, radius::MD),
                    Kind::Danger => bd(color::DANGER_SOFT, 2.0, radius::MD),
                    _ => Border::default().rounded(radius::MD),
                },
                shadow: Shadow::default(),
            };
        }

        let (bg, fg, border) = match kind {
            Kind::Primary => {
                let (bg, border) = match status {
                    Status::Hovered => (color::ACCENT_HOVER, color::ACCENT_HOVER),
                    Status::Pressed => (color::ACCENT_PRESSED, color::ACCENT_PRESSED),
                    _ => (color::ACCENT, color::ACCENT),
                };
                (bg, color::ACCENT_INK, bd(border, 2.0, radius::MD))
            }
            Kind::Secondary => {
                let bg = match status {
                    Status::Hovered => color::SURFACE_HOVER,
                    Status::Pressed => color::SURFACE_PRESSED,
                    _ => Color::TRANSPARENT,
                };
                (
                    bg,
                    color::LABEL_PRIMARY,
                    bd(color::BORDER_HAIRLINE, 2.0, radius::MD),
                )
            }
            Kind::Ghost => {
                let bg = match status {
                    Status::Hovered => color::SURFACE_HOVER,
                    Status::Pressed => color::SURFACE_PRESSED,
                    _ => Color::TRANSPARENT,
                };
                let fg = match status {
                    Status::Hovered => color::LABEL_PRIMARY,
                    _ => color::LABEL_SECONDARY,
                };
                (bg, fg, Border::default().rounded(radius::MD))
            }
            Kind::Danger => {
                let bg = match status {
                    Status::Hovered | Status::Pressed => color::DANGER_SOFT,
                    _ => Color::TRANSPARENT,
                };
                (bg, color::DANGER, bd(color::DANGER, 2.0, radius::MD))
            }
        };

        button::Style {
            background: solid(bg),
            text_color: fg,
            border,
            shadow: Shadow::default(),
        }
    }
}

/// 列表行：常态 / 悬停 / 选中三态。
pub fn row_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        use button::Status;

        let (bg, border) = if selected {
            (color::ACCENT_SOFT, bd(color::ACCENT, 2.0, radius::LG))
        } else {
            match status {
                Status::Hovered => (color::SURFACE_HOVER, bd(color::BORDER_HAIRLINE, 2.0, radius::LG)),
                Status::Pressed => (color::SURFACE_PRESSED, bd(color::BORDER_HAIRLINE, 2.0, radius::LG)),
                _ => (Color::TRANSPARENT, bd(color::BORDER_HAIRLINE, 2.0, radius::LG)),
            }
        };

        button::Style {
            background: solid(bg),
            text_color: color::LABEL_PRIMARY,
            border,
            shadow: Shadow::default(),
        }
    }
}

/// 列表行外壳（承载整行背景，内层点击区透明）：常态 / 选中 / 卡上默认三态。
pub fn row_shell(selected: bool, is_default: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| {
        if is_default {
            container::Style {
                text_color: Some(color::LABEL_PRIMARY),
                background: Some(Background::Color(color::GOLD_SOFT)),
                border: bd(color::GOLD, 2.0, radius::LG),
                shadow: Shadow::default(),
            }
        } else if selected {
            container::Style {
                text_color: Some(color::LABEL_PRIMARY),
                background: Some(Background::Color(color::ACCENT_SOFT)),
                border: bd(color::ACCENT, 2.0, radius::LG),
                shadow: Shadow::default(),
            }
        } else {
            container::Style {
                text_color: Some(color::LABEL_PRIMARY),
                background: None,
                border: bd(color::BORDER_HAIRLINE, 2.0, radius::LG),
                shadow: Shadow::default(),
            }
        }
    }
}

/// 行内点击区：完全透明，仅提供命中与悬停。
pub fn click_zone(_: &Theme, status: button::Status) -> button::Style {
    use button::Status;
    let bg = match status {
        Status::Hovered => Some(Background::Color(color::SURFACE_HOVER)),
        Status::Pressed => Some(Background::Color(color::SURFACE_PRESSED)),
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: color::LABEL_PRIMARY,
        border: Border::default().rounded(radius::SM),
        shadow: Shadow::default(),
    }
}

/// 「导出」开关 chip：开 = accent 底深字，关 = 透明浅字发丝边。
pub fn export_chip(on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        use button::Status;
        let (bg, border, fg) = if on {
            let bg = match status {
                Status::Hovered => color::ACCENT_HOVER,
                _ => color::ACCENT,
            };
            (bg, color::ACCENT, color::ACCENT_INK)
        } else {
            let bg = match status {
                Status::Hovered => color::SURFACE_HOVER,
                _ => Color::TRANSPARENT,
            };
            (bg, color::BORDER_HAIRLINE, color::LABEL_TERTIARY)
        };
        button::Style {
            background: solid(bg),
            text_color: fg,
            border: bd(border, 2.0, radius::XS),
            shadow: Shadow::default(),
        }
    }
}

/// 行内 × 删除按钮：悬停转红。
pub fn icon_danger(_: &Theme, status: button::Status) -> button::Style {
    use button::Status;
    let (bg, fg) = match status {
        Status::Hovered | Status::Pressed => (color::DANGER_SOFT, color::DANGER),
        _ => (Color::TRANSPARENT, color::LABEL_TERTIARY),
    };
    button::Style {
        background: solid(bg),
        text_color: fg,
        border: Border::default().rounded(radius::XS),
        shadow: Shadow::default(),
    }
}

/// 日志区底：略亮暗表面（内容层），发丝边，浅字。
pub fn log_panel(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::LABEL_SECONDARY),
        background: Some(Background::Color(color::SURFACE_FIELD)),
        border: bd(color::BORDER_HAIRLINE, 2.0, radius::MD),
        shadow: Shadow::default(),
    }
}

/// 提示 / 确认条底：accent 淡填充 + accent 边（暗底上仍清晰）。
pub fn notice(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::LABEL_PRIMARY),
        background: Some(Background::Color(color::ACCENT_SOFT)),
        border: bd(color::ACCENT, 2.0, radius::MD),
        shadow: Shadow::default(),
    }
}

/// 金色 chip（卡上「默认开机」标记）。
pub fn chip_gold(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(color::GOLD),
        background: Some(Background::Color(color::GOLD_SOFT)),
        border: bd(color::GOLD, 2.0, radius::XS),
        shadow: Shadow::default(),
    }
}

/// 分段控件里的单个段：选中 = accent 实心深字；未选 = 透明浅字。
pub fn segment_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        use button::Status;

        let (bg, fg) = if selected {
            (color::ACCENT, color::ACCENT_INK)
        } else {
            match status {
                Status::Hovered => (color::SURFACE_HOVER, color::LABEL_PRIMARY),
                _ => (Color::TRANSPARENT, color::LABEL_TERTIARY),
            }
        };

        button::Style {
            background: solid(bg),
            text_color: fg,
            border: Border::default().rounded(radius::SM),
            shadow: Shadow::default(),
        }
    }
}

/// 左侧导航菜单项：选中态黑白反转（近白底 + 深色字），零圆角；非选中透明。
/// 与分段控件不同——导航选中不用 accent，accent 只留给状态徽章与主 CTA。
pub fn nav_item_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        use button::Status;

        let (bg, fg) = if selected {
            (color::LABEL_PRIMARY, color::CANVAS_BASE)
        } else {
            match status {
                Status::Hovered => (color::SURFACE_HOVER, color::LABEL_PRIMARY),
                _ => (Color::TRANSPARENT, color::LABEL_TERTIARY),
            }
        };

        button::Style {
            background: solid(bg),
            text_color: fg,
            border: Border::default(),
            shadow: Shadow::default(),
        }
    }
}

/// 开关轨道：开 = accent，关 = 透明发丝边；硬边、浅方块指示器。
pub fn toggle_style(on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        use button::Status;

        let bg = if on {
            match status {
                Status::Hovered => color::ACCENT_HOVER,
                Status::Pressed => color::ACCENT_PRESSED,
                _ => color::ACCENT,
            }
        } else {
            match status {
                Status::Hovered => color::SURFACE_HOVER,
                _ => Color::TRANSPARENT,
            }
        };

        button::Style {
            background: solid(bg),
            text_color: color::ACCENT_INK,
            border: bd(color::BORDER_HAIRLINE, 2.0, radius::PILL),
            shadow: Shadow::default(),
        }
    }
}

/// 拖放区：透明底 + 强描边；hover/按压转 accent 淡填充。
pub fn drop_zone(_: &Theme, status: button::Status) -> button::Style {
    use button::Status;

    let (bg, border) = match status {
        Status::Hovered => (color::ACCENT_SOFT, bd(color::ACCENT, 2.0, radius::LG)),
        Status::Pressed => (color::ACCENT_SOFT, bd(color::ACCENT, 2.0, radius::LG)),
        _ => (Color::TRANSPARENT, bd(color::BORDER_STRONG, 2.0, radius::LG)),
    };

    button::Style {
        background: solid(bg),
        text_color: color::LABEL_SECONDARY,
        border,
        shadow: Shadow::default(),
    }
}

// ---------------------------------------------------------------- 输入 / 滑块 / 滚动

pub fn field(_: &Theme, status: text_input::Status) -> text_input::Style {
    use text_input::Status;

    let (bg, border) = match status {
        Status::Focused => (color::SURFACE_FIELD, bd(color::ACCENT, 2.0, radius::MD)),
        Status::Hovered => (color::SURFACE_FIELD, bd(color::BORDER_STRONG, 2.0, radius::MD)),
        Status::Disabled => (color::SURFACE_RAISED, bd(color::BORDER_HAIRLINE, 2.0, radius::MD)),
        _ => (color::SURFACE_FIELD, bd(color::BORDER_HAIRLINE, 2.0, radius::MD)),
    };

    text_input::Style {
        background: Background::Color(bg),
        border,
        icon: color::LABEL_TERTIARY,
        placeholder: color::LABEL_QUATERNARY,
        value: color::LABEL_PRIMARY,
        selection: color::ACCENT_SOFT,
    }
}

pub fn slider_style(_: &Theme, _status: slider::Status) -> slider::Style {
    use slider::{Handle, HandleShape, Rail, Style};

    Style {
        rail: Rail {
            backgrounds: (
                Background::Color(color::ACCENT),
                Background::Color(color::BORDER_HAIRLINE),
            ),
            width: 4.0,
            border: Border::default().rounded(0.0),
        },
        handle: Handle {
            shape: HandleShape::Rectangle {
                width: 14,
                border_radius: 0.0.into(),
            },
            background: Background::Color(color::CANVAS_TEXT),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
        },
    }
}

pub fn scrollable_style(_: &Theme, _status: scrollable::Status) -> scrollable::Style {
    use scrollable::{Rail, Scroller, Style};

    let rail = Rail {
        background: None,
        border: Border::default(),
        scroller: Scroller {
            color: rgba(237, 237, 239, 0.30),
            border: Border::default().rounded(0.0),
        },
    };

    Style {
        container: container::Style {
            text_color: None,
            background: None,
            border: Border::default(),
            shadow: Shadow::default(),
        },
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
    }
}

// ---------------------------------------------------------------- 字体

pub mod text {
    use iced::font::{Family, Stretch, Style, Weight};
    use iced::Font;

    // 系统字体：中文 PingFang SC。iced 渲染器对 Menlo 无 CJK 回退（中文渲染成
    // 方框），因此含中文的元数据/辅助文本一律用 PingFang SC，不再用 Menlo。
    const FAMILY_UI: Family = Family::Name("PingFang SC");

    const fn font(family: Family, weight: Weight) -> Font {
        Font {
            family,
            weight,
            stretch: Stretch::Normal,
            style: Style::Normal,
        }
    }

    /// 22 / Semibold — 窗口主标题（Bold 会回退到无中文字形，见规范）
    pub const DISPLAY: Font = font(FAMILY_UI, Weight::Semibold);
    /// 15 / Semibold — 分区标题
    pub const TITLE: Font = font(FAMILY_UI, Weight::Semibold);
    /// 13 / Semibold — 行名、按钮
    pub const BODY_STRONG: Font = font(FAMILY_UI, Weight::Semibold);
    /// 13 / Regular — 正文
    pub const BODY: Font = font(FAMILY_UI, Weight::Normal);
    /// 13 / Medium — 数值
    pub const NUM: Font = font(FAMILY_UI, Weight::Medium);
    /// 11 / Regular — 辅助
    pub const FOOTNOTE: Font = font(FAMILY_UI, Weight::Normal);
    /// 10 / Semibold — 分区序号
    pub const CAPTION: Font = font(FAMILY_UI, Weight::Semibold);

    pub const DISPLAY_SIZE: f32 = 22.0;
    pub const TITLE_SIZE: f32 = 15.0;
    pub const BODY_SIZE: f32 = 13.0;
    pub const FOOTNOTE_SIZE: f32 = 11.0;
    pub const CAPTION_SIZE: f32 = 10.0;
}
