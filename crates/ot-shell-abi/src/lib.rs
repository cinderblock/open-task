//! The C ABI between open-task and a toolkit plugin, as Rust types.
//!
//! `include/ot_shell.h` is the definition; this is its mirror, for the host
//! (`ot-shell-linux`) and for a plugin written in Rust (`ot-shell-gtk`). The draw
//! types are `ot_paint`'s own, which are `#[repr(C)]`; the assertions below and
//! the header's check that both sides lay them out alike, and a test compiles the
//! header against offsets taken from these types when a C compiler is at hand.
//!
//! The function-pointer tables are plain data. Calling through them is the
//! caller's business and is `unsafe` there.

use std::ffi::{c_char, c_int, c_void};
use std::mem::{offset_of, size_of};

pub use ot_paint::{Color, DrawCmd, Point, Rect, Span, TextCmd, TextStyle};

/// Bumped on any change to the header.
pub const ABI_VERSION: u32 = 3;

/// The symbol a plugin exports: `const OtPlugin *ot_shell_plugin(void)`.
pub const PLUGIN_SYMBOL: &[u8] = b"ot_shell_plugin\0";

/// `OtShellPluginFn`.
pub type PluginFn = unsafe extern "C" fn() -> *const Plugin;

/// `OtRaster`: a run of chart commands the host drew itself, which the plugin
/// draws as this image instead of the commands `first..end`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Raster {
    pub first: u32,
    pub end: u32,
    /// Where, in DIPs: whole device pixels at the frame's scale.
    pub rect: Rect,
    /// In device pixels.
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA, `width * 4` bytes a row, top row first.
    pub pixels: *const u8,
}

/// `OtDisplayList`: one frame, borrowed from the host until the plugin next
/// calls into it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DisplayList {
    pub cmds: *const DrawCmd,
    pub cmd_count: usize,
    pub points: *const Point,
    pub point_count: usize,
    pub strings: *const c_char,
    pub strings_len: usize,
    /// Runs of commands drawn as images, in command order, not overlapping.
    pub rasters: *const Raster,
    pub raster_count: usize,
    /// Where this frame differs from the last, in whole DIPs, unless `whole`
    /// (for a plugin with [`feature::PARTIAL_PAINT`]; else always `whole`).
    pub damage: *const Rect,
    pub damage_count: usize,
    pub whole: bool,
}

impl DisplayList {
    pub const EMPTY: Self = Self {
        cmds: std::ptr::null(),
        cmd_count: 0,
        points: std::ptr::null(),
        point_count: 0,
        strings: std::ptr::null(),
        strings_len: 0,
        rasters: std::ptr::null(),
        raster_count: 0,
        damage: std::ptr::null(),
        damage_count: 0,
        whole: true,
    };

    /// Point at `list`'s commands and arenas, at `rasters`, and at `damage`
    /// (`None` for a whole frame), which must outlive the use of this as much as
    /// `list` does.
    #[must_use]
    pub fn of(list: &ot_paint::DisplayList, rasters: &[Raster], damage: Option<&[Rect]>) -> Self {
        let cmds = list.cmds();
        let points = list.point_arena();
        let strings = list.string_arena();
        Self {
            cmds: cmds.as_ptr(),
            cmd_count: cmds.len(),
            points: points.as_ptr(),
            point_count: points.len(),
            strings: strings.as_ptr().cast(),
            strings_len: strings.len(),
            rasters: rasters.as_ptr(),
            raster_count: rasters.len(),
            damage: damage.map_or(std::ptr::null(), <[Rect]>::as_ptr),
            damage_count: damage.map_or(0, <[Rect]>::len),
            whole: damage.is_none(),
        }
    }
}

/// `OtEventKind`.
pub mod event {
    pub const RESIZE: u32 = 0;
    pub const SCALE: u32 = 1;
    pub const MOUSE_MOVE: u32 = 2;
    pub const MOUSE_LEAVE: u32 = 3;
    pub const MOUSE_DOWN: u32 = 4;
    pub const MOUSE_UP: u32 = 5;
    pub const WHEEL: u32 = 6;
    pub const KEY: u32 = 7;
    pub const TEXT: u32 = 8;
    pub const CONTEXT_MENU: u32 = 9;
    pub const CLOSE_REQUEST: u32 = 10;
    pub const FOCUS: u32 = 11;
    pub const VISIBILITY: u32 = 12;
    pub const THEME: u32 = 13;
    pub const ANIMATIONS: u32 = 14;
}

/// `OtFeature` bits.
pub mod feature {
    pub const KEEP_ABOVE: u32 = 1;
    pub const ANIMATIONS: u32 = 2;
    pub const PARTIAL_PAINT: u32 = 4;
}

/// `OtMouseButton`.
pub mod button {
    pub const LEFT: u32 = 0;
    pub const RIGHT: u32 = 1;
    pub const MIDDLE: u32 = 2;
}

/// `OtKey`.
pub mod key {
    pub const NONE: u32 = 0;
    pub const UP: u32 = 1;
    pub const DOWN: u32 = 2;
    pub const PAGE_UP: u32 = 3;
    pub const PAGE_DOWN: u32 = 4;
    pub const HOME: u32 = 5;
    pub const END: u32 = 6;
    pub const LEFT: u32 = 7;
    pub const RIGHT: u32 = 8;
    pub const BACKSPACE: u32 = 9;
    pub const DELETE: u32 = 10;
    pub const ESCAPE: u32 = 11;
    pub const ENTER: u32 = 12;
    pub const TAB: u32 = 13;
    pub const MENU: u32 = 14;
    pub const CHARACTER: u32 = 15;
}

/// `OtModifier` bits.
pub mod modifier {
    pub const CTRL: u32 = 1;
    pub const SHIFT: u32 = 2;
    pub const ALT: u32 = 4;
    pub const SUPER: u32 = 8;
}

/// `OtMenuKind`.
pub mod menu {
    pub const ITEM: u32 = 0;
    pub const SUBMENU: u32 = 1;
    pub const SUBMENU_END: u32 = 2;
    pub const SEPARATOR: u32 = 3;
}

/// `OtNoticeKind`.
pub mod notice {
    pub const INFO: u32 = 0;
    pub const WARNING: u32 = 1;
    pub const ERROR: u32 = 2;
}

/// `OtCursor`.
pub mod cursor {
    pub const ARROW: u32 = 0;
    pub const RESIZE_COLUMN: u32 = 1;
    pub const TEXT: u32 = 2;
    pub const CROSSHAIR: u32 = 3;
}

/// `OtLogLevel`.
pub mod log {
    pub const ERROR: u32 = 0;
    pub const WARN: u32 = 1;
    pub const INFO: u32 = 2;
    pub const DEBUG: u32 = 3;
}

/// `OtEvent`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Event {
    pub kind: u32,
    pub button: u32,
    pub key: u32,
    pub modifiers: u32,
    pub codepoint: u32,
    pub flag: u32,
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
}

/// `OtMenuItem`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MenuItem {
    pub kind: u32,
    pub enabled: bool,
    pub checked: bool,
    pub id: u64,
    pub label: *const c_char,
    pub label_len: usize,
    /// `OtIcon` ([`ot_paint::Icon`] as `u32`), or [`ICON_NONE`].
    pub icon: u32,
}

/// `OT_ICON_NONE`: a menu item with no icon.
pub const ICON_NONE: u32 = u32::MAX;

/// `OtHost`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Host {
    pub ctx: *mut c_void,
    pub event: unsafe extern "C" fn(ctx: *mut c_void, event: *const Event),
    pub paint: unsafe extern "C" fn(ctx: *mut c_void, scale: f32, out: *mut DisplayList),
    pub animating: unsafe extern "C" fn(ctx: *mut c_void) -> bool,
    pub wake: unsafe extern "C" fn(ctx: *mut c_void),
    pub timer: unsafe extern "C" fn(ctx: *mut c_void, id: u32),
    pub menu_chosen: unsafe extern "C" fn(ctx: *mut c_void, token: u64, chosen: bool, id: u64),
    pub confirmed: unsafe extern "C" fn(ctx: *mut c_void, token: u64, yes: bool),
    pub file_chosen:
        unsafe extern "C" fn(ctx: *mut c_void, token: u64, path: *const c_char, path_len: usize),
    pub run_entered: unsafe extern "C" fn(
        ctx: *mut c_void,
        token: u64,
        command: *const c_char,
        command_len: usize,
        elevated: bool,
    ),
    pub log: unsafe extern "C" fn(ctx: *mut c_void, level: u32, message: *const c_char, len: usize),
}

/// `OtStartup`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Startup {
    pub title: *const c_char,
    pub title_len: usize,
    pub app_id: *const c_char,
    pub app_id_len: usize,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
    pub dark: bool,
    pub argc: c_int,
    pub argv: *mut *mut c_char,
}

/// `OtPlugin`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Plugin {
    pub abi_version: u32,
    pub struct_size: u32,
    pub toolkit: unsafe extern "C" fn() -> *const c_char,
    pub run: unsafe extern "C" fn(
        host: *const Host,
        startup: *const Startup,
        error: *mut c_char,
        error_cap: usize,
    ) -> i32,
    pub wake: unsafe extern "C" fn(),
    pub invalidate: unsafe extern "C" fn(),
    pub set_title: unsafe extern "C" fn(title: *const c_char, len: usize),
    pub set_cursor: unsafe extern "C" fn(cursor: u32),
    pub set_keep_above: unsafe extern "C" fn(on: bool),
    pub set_dark: unsafe extern "C" fn(dark: bool),
    pub present: unsafe extern "C" fn(),
    pub minimize: unsafe extern "C" fn(),
    pub hide: unsafe extern "C" fn(),
    pub close: unsafe extern "C" fn(),
    pub set_timer: unsafe extern "C" fn(id: u32, ms: u32),
    pub show_menu:
        unsafe extern "C" fn(token: u64, x: f32, y: f32, items: *const MenuItem, count: usize),
    pub message: unsafe extern "C" fn(kind: u32, text: *const c_char, len: usize),
    pub confirm: unsafe extern "C" fn(token: u64, question: *const c_char, len: usize),
    #[allow(clippy::type_complexity)]
    pub save_file: unsafe extern "C" fn(
        token: u64,
        title: *const c_char,
        title_len: usize,
        default_name: *const c_char,
        default_name_len: usize,
        filter_name: *const c_char,
        filter_name_len: usize,
        pattern: *const c_char,
        pattern_len: usize,
    ),
    pub run_dialog: unsafe extern "C" fn(token: u64, initial: *const c_char, len: usize),
    pub copy_text: unsafe extern "C" fn(text: *const c_char, len: usize),
    pub features: unsafe extern "C" fn() -> u32,
}

// The header asserts the same numbers; a change on one side alone fails to build.
const _: () = {
    assert!(size_of::<Point>() == 8);
    assert!(size_of::<Rect>() == 16);
    assert!(size_of::<Color>() == 16);
    assert!(size_of::<Span>() == 8);
    assert!(size_of::<TextStyle>() == 16);
    assert!(size_of::<TextCmd>() == 68);
    assert!(offset_of!(TextCmd, color) == 40);
    assert!(offset_of!(TextCmd, caret) == 66);
    assert!(size_of::<DrawCmd>() == 72);
    assert!(size_of::<Event>() == 40);
    assert!(size_of::<Raster>() == 40);
    assert!(offset_of!(Raster, pixels) == 32);
};

/// The header, for plugins built from this repository (`ot-shell-qt`'s build
/// script includes it from here).
pub const HEADER: &str = include_str!("../include/ot_shell.h");

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// `assert(offsetof(T, f) == n)` lines for C, from this side's layout.
    fn c_checks() -> String {
        let mut c = String::new();
        let mut check = |what: &str, value: usize| {
            let _ = writeln!(c, "OT_CHECK({what} == {value});");
        };
        check("sizeof(OtEvent)", size_of::<Event>());
        check("offsetof(OtEvent, flag)", offset_of!(Event, flag));
        check("offsetof(OtEvent, dy)", offset_of!(Event, dy));
        check("sizeof(OtMenuItem)", size_of::<MenuItem>());
        check("offsetof(OtMenuItem, id)", offset_of!(MenuItem, id));
        check(
            "offsetof(OtMenuItem, label_len)",
            offset_of!(MenuItem, label_len),
        );
        check("offsetof(OtMenuItem, icon)", offset_of!(MenuItem, icon));
        check("OT_ICON_COUNT", ot_paint::Icon::ALL.len());
        check("OT_EVENT_ANIMATIONS", event::ANIMATIONS as usize);
        check("OT_FEATURE_KEEP_ABOVE", feature::KEEP_ABOVE as usize);
        check("OT_FEATURE_ANIMATIONS", feature::ANIMATIONS as usize);
        check("OT_FEATURE_PARTIAL_PAINT", feature::PARTIAL_PAINT as usize);
        check("sizeof(OtDisplayList)", size_of::<DisplayList>());
        check(
            "offsetof(OtDisplayList, rasters)",
            offset_of!(DisplayList, rasters),
        );
        check(
            "offsetof(OtDisplayList, damage)",
            offset_of!(DisplayList, damage),
        );
        check(
            "offsetof(OtDisplayList, whole)",
            offset_of!(DisplayList, whole),
        );
        check("sizeof(OtRaster)", size_of::<Raster>());
        check("offsetof(OtRaster, rect)", offset_of!(Raster, rect));
        check("offsetof(OtRaster, width)", offset_of!(Raster, width));
        check("sizeof(OtHost)", size_of::<Host>());
        check("offsetof(OtHost, log)", offset_of!(Host, log));
        check("sizeof(OtStartup)", size_of::<Startup>());
        check("offsetof(OtStartup, dark)", offset_of!(Startup, dark));
        check("offsetof(OtStartup, argv)", offset_of!(Startup, argv));
        check("sizeof(OtPlugin)", size_of::<Plugin>());
        check("offsetof(OtPlugin, run)", offset_of!(Plugin, run));
        check(
            "offsetof(OtPlugin, copy_text)",
            offset_of!(Plugin, copy_text),
        );
        check("offsetof(OtPlugin, features)", offset_of!(Plugin, features));
        check("sizeof(OtTextCmd)", size_of::<TextCmd>());
        check("offsetof(OtTextCmd, style)", offset_of!(TextCmd, style));
        check("offsetof(OtTextCmd, halign)", offset_of!(TextCmd, halign));
        check(
            "offsetof(OtTextCmd, ellipsis)",
            offset_of!(TextCmd, ellipsis),
        );
        check("sizeof(OtDrawCmd)", size_of::<DrawCmd>());
        // Each variant's fields sit where the union's members put them.
        let variants: [(&str, DrawCmd); 4] = [
            (
                "u.line.width",
                DrawCmd::Line {
                    from: Point::new(0.0, 0.0),
                    to: Point::new(0.0, 0.0),
                    color: Color::BLACK,
                    width: 7.5,
                },
            ),
            (
                "u.stroke_round_rect.width",
                DrawCmd::StrokeRoundRect {
                    rect: Rect::ZERO,
                    radius: 0.0,
                    color: Color::BLACK,
                    width: 7.5,
                },
            ),
            (
                "u.graph.width",
                DrawCmd::Graph {
                    points: Span::default(),
                    color: Color::BLACK,
                    width: 7.5,
                },
            ),
            (
                "u.icon.size",
                DrawCmd::Icon {
                    icon: ot_paint::Icon::Menu,
                    rect: Rect::ZERO,
                    size: 7.5,
                    color: Color::BLACK,
                },
            ),
        ];
        for (field, cmd) in variants {
            let _ = writeln!(
                c,
                "OT_CHECK(offsetof(OtDrawCmd, {field}) == {});",
                float_at(&cmd, 7.5)
            );
        }
        c
    }

    /// Where the only `value` in a command sits, found by looking at its bytes:
    /// the layout as the compiler made it, not as written down.
    fn float_at(cmd: &DrawCmd, value: f32) -> usize {
        let size = size_of::<DrawCmd>();
        // SAFETY: reading the bytes of a plain, initialized value; padding bytes
        // may be anything, and a match there is ruled out by the uniqueness check.
        let bytes =
            unsafe { std::slice::from_raw_parts(std::ptr::from_ref(cmd).cast::<u8>(), size) };
        let needle = value.to_ne_bytes();
        let found: Vec<usize> = (0..=size - 4)
            .filter(|&i| bytes[i..i + 4] == needle)
            .collect();
        assert_eq!(found.len(), 1, "{value} appears once");
        found[0]
    }

    #[test]
    fn tags_follow_the_header() {
        // The tag is the first u32 and counts the variants in order.
        let tag = |cmd: &DrawCmd| {
            // SAFETY: a repr(C, u32) enum starts with its u32 tag.
            unsafe { *std::ptr::from_ref(cmd).cast::<u32>() }
        };
        assert_eq!(tag(&DrawCmd::Clear(Color::BLACK)), 0);
        assert_eq!(
            tag(&DrawCmd::Text(TextCmd {
                text: Span::default(),
                rect: Rect::ZERO,
                style: TextStyle::default(),
                color: Color::BLACK,
                halign: ot_paint::HAlign::Left,
                valign: ot_paint::VAlign::Middle,
                ellipsis: false,
                field: false,
                caret: false,
            })),
            10
        );
        assert_eq!(tag(&DrawCmd::PopClip), 14);
        assert!(HEADER.contains("OT_CMD_TEXT,\n") || HEADER.contains("OT_CMD_TEXT,\r\n"));
    }

    /// Compile the header with the offsets this side computes, when a C
    /// compiler is available (`cc`, as on the Linux CI runners).
    #[test]
    fn the_header_agrees_with_these_types() {
        let Some(cc) = ["cc", "gcc", "clang"].into_iter().find(|cc| {
            std::process::Command::new(cc)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        }) else {
            eprintln!("no C compiler; skipping the header check");
            return;
        };
        let dir = std::env::temp_dir().join(format!("ot-shell-abi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let src = dir.join("check.c");
        let include = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
        let code = format!(
            "#include \"ot_shell.h\"\n#define OT_CHECK(c) _Static_assert(c, #c)\n{}",
            c_checks()
        );
        std::fs::write(&src, code).expect("write");
        let out = std::process::Command::new(cc)
            .args(["-std=c11", "-fsyntax-only", "-Wall", "-Werror", "-I"])
            .arg(&include)
            .arg(&src)
            .output()
            .expect("run the C compiler");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
