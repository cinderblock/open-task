//! Colors, type styles and metrics.
//!
//! Surfaces are translucent so a system backdrop (Mica on Windows 11) shows through.
//! When no backdrop is available the shell tells the view to clear to `bg_solid`
//! first, and the same translucent surfaces read correctly over it.

use ot_paint::{Color, FontWeight, TextStyle};

#[derive(Debug, Clone)]
pub struct Theme {
    /// Opaque fallback background for platforms without a compositor backdrop.
    pub bg_solid: Color,
    /// Card and header fill.
    pub surface: Color,
    pub surface_border: Color,

    pub text: Color,
    pub text_dim: Color,
    pub accent: Color,

    /// Series colors.
    pub cpu: Color,
    pub memory: Color,

    pub row_alt: Color,
    pub row_hover: Color,
    pub row_selected: Color,
    pub grid: Color,
    /// Base color for per-cell heat tint; alpha is scaled by intensity.
    pub heat: Color,
    pub scrollbar: Color,

    pub cell: TextStyle,
    pub cell_num: TextStyle,
    pub header: TextStyle,
    pub title: TextStyle,
    pub big: TextStyle,
    pub small: TextStyle,

    pub row_h: f32,
    pub header_h: f32,
    /// Horizontal padding inside cells and cards.
    pub pad: f32,
    /// Gap between top-level regions.
    pub gap: f32,
    pub card_radius: f32,
}

impl Theme {
    #[must_use]
    pub fn dark() -> Self {
        let cell = TextStyle::ui(13.0);
        Self {
            bg_solid: Color::hex(0x20_20_20),
            surface: Color::rgba(1.0, 1.0, 1.0, 0.045),
            surface_border: Color::rgba(1.0, 1.0, 1.0, 0.08),
            text: Color::hex(0xF3_F3_F3),
            text_dim: Color::rgba(1.0, 1.0, 1.0, 0.62),
            accent: Color::hex(0x60_CD_FF),
            cpu: Color::hex(0x60_CD_FF),
            memory: Color::hex(0xC5_9C_FF),
            row_alt: Color::rgba(1.0, 1.0, 1.0, 0.025),
            row_hover: Color::rgba(1.0, 1.0, 1.0, 0.06),
            row_selected: Color::hex(0x60_CD_FF).with_alpha(0.22),
            grid: Color::rgba(1.0, 1.0, 1.0, 0.07),
            heat: Color::hex(0xFF_B4_54),
            scrollbar: Color::rgba(1.0, 1.0, 1.0, 0.25),
            cell,
            cell_num: cell.tabular(),
            header: TextStyle::ui(12.0).weight(FontWeight::SemiBold),
            title: TextStyle::ui(12.0).weight(FontWeight::SemiBold),
            big: TextStyle::ui(22.0).weight(FontWeight::SemiBold).tabular(),
            small: TextStyle::ui(11.0).tabular(),
            row_h: 24.0,
            header_h: 28.0,
            pad: 8.0,
            gap: 8.0,
            card_radius: 6.0,
        }
    }

    #[must_use]
    pub fn light() -> Self {
        let cell = TextStyle::ui(13.0);
        Self {
            bg_solid: Color::hex(0xF3_F3_F3),
            surface: Color::rgba(1.0, 1.0, 1.0, 0.7),
            surface_border: Color::rgba(0.0, 0.0, 0.0, 0.08),
            text: Color::hex(0x1B_1B_1B),
            text_dim: Color::rgba(0.0, 0.0, 0.0, 0.6),
            accent: Color::hex(0x00_5F_B8),
            cpu: Color::hex(0x00_5F_B8),
            memory: Color::hex(0x74_3F_C4),
            row_alt: Color::rgba(0.0, 0.0, 0.0, 0.025),
            row_hover: Color::rgba(0.0, 0.0, 0.0, 0.05),
            row_selected: Color::hex(0x00_5F_B8).with_alpha(0.18),
            grid: Color::rgba(0.0, 0.0, 0.0, 0.08),
            heat: Color::hex(0xE0_7A_00),
            scrollbar: Color::rgba(0.0, 0.0, 0.0, 0.3),
            cell,
            cell_num: cell.tabular(),
            header: TextStyle::ui(12.0).weight(FontWeight::SemiBold),
            title: TextStyle::ui(12.0).weight(FontWeight::SemiBold),
            big: TextStyle::ui(22.0).weight(FontWeight::SemiBold).tabular(),
            small: TextStyle::ui(11.0).tabular(),
            row_h: 24.0,
            header_h: 28.0,
            pad: 8.0,
            gap: 8.0,
            card_radius: 6.0,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
