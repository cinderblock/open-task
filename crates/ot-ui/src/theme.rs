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
    /// The hover line across the charts.
    pub crosshair: Color,
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

    /// Tree mode: horizontal indent per depth level.
    pub indent: f32,
    /// Tree mode: width of the expand/collapse box that precedes every name, at
    /// every level, so names at one depth line up whether or not they have children.
    pub expander_w: f32,
    /// Indent guides in tree mode. The active color marks the selected row's sibling
    /// block, so the eye can follow the selection to its parent.
    pub tree_guide: Color,
    pub tree_guide_active: Color,

    /// Toolbar strip between the cards and the table.
    pub toolbar_h: f32,
    /// Segmented-control fills.
    pub button_hover: Color,
    pub button_active: Color,

    /// Text field fill; the border is `surface_border`, or `accent` when focused.
    pub input_bg: Color,
    /// Width of the search field in the toolbar.
    pub search_w: f32,

    /// Efficiency cores on a hybrid processor; performance cores use `cpu`.
    pub cpu_efficiency: Color,
    /// Disk activity and reads; writes use the lighter `disk_write`.
    pub disk: Color,
    pub disk_write: Color,
    /// Network traffic received; sent uses the lighter `network_send`.
    pub network: Color,
    pub network_send: Color,
    /// Modified memory: written, waiting to reach disk before it can be reused.
    pub memory_modified: Color,
    /// A headline number in a page's statistics.
    pub stat: TextStyle,
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
            crosshair: Color::rgba(1.0, 1.0, 1.0, 0.35),
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
            indent: 16.0,
            expander_w: 16.0,
            tree_guide: Color::rgba(1.0, 1.0, 1.0, 0.10),
            tree_guide_active: Color::hex(0x60_CD_FF).with_alpha(0.55),
            toolbar_h: 30.0,
            button_hover: Color::rgba(1.0, 1.0, 1.0, 0.06),
            button_active: Color::hex(0x60_CD_FF).with_alpha(0.22),
            input_bg: Color::rgba(0.0, 0.0, 0.0, 0.18),
            search_w: 220.0,
            cpu_efficiency: Color::hex(0x4F_C3_C0),
            disk: Color::hex(0x6C_CB_5F),
            disk_write: Color::hex(0xB8_E8_B0),
            network: Color::hex(0xF0_A0_4B),
            network_send: Color::hex(0xF8_D4_A8),
            memory_modified: Color::hex(0xFF_B4_54),
            stat: TextStyle::ui(17.0).weight(FontWeight::SemiBold).tabular(),
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
            crosshair: Color::rgba(0.0, 0.0, 0.0, 0.35),
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
            indent: 16.0,
            expander_w: 16.0,
            tree_guide: Color::rgba(0.0, 0.0, 0.0, 0.10),
            tree_guide_active: Color::hex(0x00_5F_B8).with_alpha(0.5),
            toolbar_h: 30.0,
            button_hover: Color::rgba(0.0, 0.0, 0.0, 0.05),
            button_active: Color::hex(0x00_5F_B8).with_alpha(0.16),
            input_bg: Color::rgba(1.0, 1.0, 1.0, 0.75),
            search_w: 220.0,
            cpu_efficiency: Color::hex(0x03_83_87),
            disk: Color::hex(0x10_7C_10),
            disk_write: Color::hex(0x6B_B8_6B),
            network: Color::hex(0xC2_5E_00),
            network_send: Color::hex(0xE8_A8_65),
            memory_modified: Color::hex(0xC2_5E_00),
            stat: TextStyle::ui(17.0).weight(FontWeight::SemiBold).tabular(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
