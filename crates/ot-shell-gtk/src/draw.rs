//! The display list drawn into a `GtkSnapshot`.
//!
//! Fills, rounded fills, outlines and clips are GSK nodes, which the renderer
//! draws on the GPU. Text is a Pango layout appended as a text node, laid out
//! once per string and style and kept while it is drawn. Icons are the icon
//! theme's symbolic icons, recoloured. Lines that are not axis-aligned,
//! polylines, polygons and chart bands are Cairo: GTK 4.6 has no path node
//! (`GskPath` came in 4.14), so each run of them in a row becomes one Cairo node
//! covering what they touch, rather than one node apiece. A run of chart shapes
//! the host has drawn itself (the CPU choice of chart drawing) comes as an image
//! ([`abi::Raster`]), drawn as a texture in place of its commands.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use gtk::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use gtk4 as gtk;
use ot_paint::{
    Color, DrawCmd, FontFamily, FontWeight, HAlign, Icon, Point, Rect, Span, TextCmd, VAlign,
};
use ot_shell_abi::{self as abi, DisplayList as AbiList};

/// A layout not drawn for this many frames is dropped.
const LAYOUT_TTL_FRAMES: u64 = 120;
/// The caret's width in DIPs, as on Windows.
const CARET_W: f32 = 1.0;

/// One frame, borrowed from the host.
pub struct Frame<'a> {
    cmds: &'a [DrawCmd],
    points: &'a [Point],
    strings: &'a str,
    rasters: &'a [abi::Raster],
}

impl std::fmt::Debug for Frame<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("cmds", &self.cmds.len())
            .field("points", &self.points.len())
            .field("strings", &self.strings.len())
            .field("rasters", &self.rasters.len())
            .finish()
    }
}

impl Frame<'_> {
    /// # Safety
    /// `list` is as the host's paint callback left it: each pointer valid for its
    /// count, the strings UTF-8.
    pub unsafe fn from_abi(list: &AbiList) -> Frame<'_> {
        // SAFETY: as the caller promises; empty when a pointer is NULL.
        unsafe {
            let cmds = if list.cmds.is_null() {
                &[][..]
            } else {
                std::slice::from_raw_parts(list.cmds, list.cmd_count)
            };
            let points = if list.points.is_null() {
                &[][..]
            } else {
                std::slice::from_raw_parts(list.points, list.point_count)
            };
            let strings = if list.strings.is_null() {
                ""
            } else {
                let bytes = std::slice::from_raw_parts(list.strings.cast::<u8>(), list.strings_len);
                std::str::from_utf8(bytes).unwrap_or("")
            };
            let rasters = if list.rasters.is_null() {
                &[][..]
            } else {
                std::slice::from_raw_parts(list.rasters, list.raster_count)
            };
            Frame {
                cmds,
                points,
                strings,
                rasters,
            }
        }
    }

    /// How many commands the frame has.
    pub fn len(&self) -> usize {
        self.cmds.len()
    }

    fn points(&self, s: Span) -> &[Point] {
        let (a, n) = (s.start as usize, s.len as usize);
        self.points.get(a..a + n).unwrap_or(&[])
    }

    fn str(&self, s: Span) -> &str {
        let (a, n) = (s.start as usize, s.len as usize);
        self.strings.get(a..a + n).unwrap_or("")
    }
}

/// `OT_FRAME_STATS`: the time spent turning lists into snapshot nodes, logged
/// through the host every few seconds. GSK's own rendering comes after and is
/// not counted.
#[derive(Debug)]
pub struct DrawStats {
    on: bool,
    since: std::time::Instant,
    frames: u32,
    cmds: usize,
    time: std::time::Duration,
}

impl Default for DrawStats {
    fn default() -> Self {
        Self {
            on: std::env::var_os("OT_FRAME_STATS").is_some(),
            since: std::time::Instant::now(),
            frames: 0,
            cmds: 0,
            time: std::time::Duration::ZERO,
        }
    }
}

impl DrawStats {
    pub fn add(&mut self, time: std::time::Duration, cmds: usize) {
        if !self.on {
            return;
        }
        self.frames += 1;
        self.cmds += cmds;
        self.time += time;
        let elapsed = self.since.elapsed();
        if elapsed.as_secs() >= 5 {
            let n = self.frames.max(1);
            crate::plugin::log(
                ot_shell_abi::log::INFO,
                &format!(
                    "snapshots: {} frames, {:.1} per second, {} commands and {} us each",
                    self.frames,
                    f64::from(self.frames) / elapsed.as_secs_f64(),
                    self.cmds / n as usize,
                    (self.time / n).as_micros()
                ),
            );
            *self = Self::default();
        }
    }
}

struct Cached {
    layout: pango::Layout,
    /// Logical size in DIPs.
    w: f32,
    h: f32,
    last_used: u64,
}

#[derive(Default)]
pub struct Painter {
    layouts: HashMap<u64, Cached>,
    frame: u64,
    /// The desktop's interface font family, read from the widget's Pango context.
    ui_family: Option<String>,
    icons: HashMap<(Icon, u32, i32), Option<gtk::IconPaintable>>,
}

impl std::fmt::Debug for Painter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Painter")
            .field("layouts", &self.layouts.len())
            .field("frame", &self.frame)
            .field("ui_family", &self.ui_family)
            .finish_non_exhaustive()
    }
}

fn rgba(c: Color) -> gdk::RGBA {
    gdk::RGBA::new(c.r, c.g, c.b, c.a)
}

fn grect(r: Rect) -> graphene::Rect {
    graphene::Rect::new(r.x, r.y, r.w.max(0.0), r.h.max(0.0))
}

fn rounded(r: Rect, radius: f32) -> gsk::RoundedRect {
    let radius = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    gsk::RoundedRect::from_rect(grect(r), radius)
}

/// The freedesktop symbolic icons for each meaning, best first: icon themes
/// differ in which they ship (Adwaita has no `utilities-system-monitor`).
fn icon_names(icon: Icon) -> &'static [&'static str] {
    match icon {
        Icon::Menu => &["open-menu-symbolic"],
        Icon::Summary => &["view-grid-symbolic", "view-app-grid-symbolic"],
        Icon::Processes => &["view-list-symbolic", "view-list-bullet-symbolic"],
        Icon::Performance => &[
            "utilities-system-monitor-symbolic",
            "org.gnome.SystemMonitor-symbolic",
            "power-profile-performance-symbolic",
        ],
        Icon::Settings => &["emblem-system-symbolic", "preferences-system-symbolic"],
        Icon::Update => &[
            "view-refresh-symbolic",
            "software-update-available-symbolic",
        ],
        Icon::Users => &["system-users-symbolic", "avatar-default-symbolic"],
        Icon::Services => &["applications-system-symbolic", "emblem-system-symbolic"],
        Icon::Startup => &["system-shutdown-symbolic", "system-run-symbolic"],
        Icon::Connections => &["network-workgroup-symbolic", "network-wired-symbolic"],
        Icon::Apps => &[
            "applications-other-symbolic",
            "view-app-grid-symbolic",
            "package-x-generic-symbolic",
        ],
        Icon::System => &["computer-symbolic"],
        Icon::Play => &["media-playback-start-symbolic"],
        Icon::Pause => &["media-playback-pause-symbolic"],
        Icon::Previous => &["media-skip-backward-symbolic"],
        Icon::Next => &["media-skip-forward-symbolic"],
        Icon::Tree => &["view-list-tree-symbolic", "view-list-symbolic"],
        Icon::EndTask => &["window-close-symbolic", "process-stop-symbolic"],
        Icon::EndTree => &["process-stop-symbolic", "edit-delete-symbolic"],
        Icon::Restart => &["view-refresh-symbolic"],
        Icon::Efficiency => &["power-profile-power-saver-symbolic", "battery-symbolic"],
        Icon::Priority => &[
            "power-profile-balanced-symbolic",
            "power-profile-performance-symbolic",
        ],
        Icon::Affinity => &["application-x-firmware-symbolic", "computer-symbolic"],
        Icon::SwitchTo => &["go-jump-symbolic", "window-new-symbolic"],
        Icon::OpenFolder => &["folder-open-symbolic"],
        Icon::SearchOnline => &["system-search-symbolic", "web-browser-symbolic"],
        Icon::Properties => &[
            "document-properties-symbolic",
            "dialog-information-symbolic",
        ],
        Icon::Copy => &["edit-copy-symbolic"],
        Icon::Dump => &["document-save-symbolic"],
        Icon::WaitChain => &["insert-link-symbolic"],
        Icon::Stopwatch => &["alarm-symbolic", "preferences-system-time-symbolic"],
    }
}

/// Whether a command is drawn with Cairo, so a run of them can share a node.
// Exact equality is the point: a line the view drew level has the same y at
// both ends, to the bit.
#[allow(clippy::float_cmp)]
fn is_path(cmd: &DrawCmd) -> bool {
    match cmd {
        DrawCmd::Line { from, to, .. } => from.x != to.x && from.y != to.y,
        DrawCmd::Polyline { .. }
        | DrawCmd::FillPolygon { .. }
        | DrawCmd::Band { .. }
        | DrawCmd::Graph { .. } => true,
        _ => false,
    }
}

/// Where the run of path commands starting at `start` ends: at the first command
/// that is not one, or at `stop`.
fn path_run_end(cmds: &[DrawCmd], start: usize, stop: usize) -> usize {
    let stop = stop.clamp(start + 1, cmds.len());
    cmds[start..stop]
        .iter()
        .position(|c| !is_path(c))
        .map_or(stop, |n| start + n)
}

/// The smallest rectangle around some points, grown by `pad` each way.
fn bounds_of<'a>(pts: impl IntoIterator<Item = &'a Point>, pad: f32, acc: &mut Option<Rect>) {
    for p in pts {
        let r = Rect::new(p.x - pad, p.y - pad, pad * 2.0, pad * 2.0);
        *acc = Some(match *acc {
            None => r,
            Some(a) => {
                let x0 = a.x.min(r.x);
                let y0 = a.y.min(r.y);
                let x1 = (a.x + a.w).max(r.x + r.w);
                let y1 = (a.y + a.h).max(r.y + r.h);
                Rect::new(x0, y0, x1 - x0, y1 - y0)
            }
        });
    }
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

impl Painter {
    pub fn draw(&mut self, s: &gtk::Snapshot, frame: &Frame<'_>, widget: &gtk::Widget) {
        self.frame += 1;
        if self.ui_family.is_none() {
            self.ui_family = widget
                .pango_context()
                .font_description()
                .and_then(|d| d.family().map(|f| f.to_string()));
        }
        let full = Rect::new(0.0, 0.0, widget.width() as f32, widget.height() as f32);
        // The clip rectangles pushed, for bounding Cairo runs.
        let mut clips: Vec<Rect> = vec![full];
        let cmds = frame.cmds;
        let mut rasters = frame.rasters.iter().peekable();
        let mut i = 0;
        while i < cmds.len() {
            if let Some(end) = draw_raster_at(s, &mut rasters, i) {
                i = end;
                continue;
            }
            if is_path(&cmds[i]) {
                // A path run stops where an image's run starts.
                let stop = rasters.peek().map_or(cmds.len(), |r| r.first as usize);
                let end = path_run_end(cmds, i, stop);
                draw_paths(s, frame, &cmds[i..end], *clips.last().unwrap_or(&full));
                i = end;
                continue;
            }
            match cmds[i] {
                DrawCmd::Clear(c) => s.append_color(&rgba(c), &grect(full)),
                DrawCmd::FillRect { rect, color } => s.append_color(&rgba(color), &grect(rect)),
                DrawCmd::FillRoundRect {
                    rect,
                    radius,
                    color,
                } => {
                    s.push_rounded_clip(&rounded(rect, radius));
                    s.append_color(&rgba(color), &grect(rect));
                    s.pop();
                }
                DrawCmd::StrokeRect { rect, color, width } => {
                    s.append_border(&rounded(rect, 0.0), &[width; 4], &[rgba(color); 4]);
                }
                DrawCmd::StrokeRoundRect {
                    rect,
                    radius,
                    color,
                    width,
                } => s.append_border(&rounded(rect, radius), &[width; 4], &[rgba(color); 4]),
                // Only axis-aligned lines get here: a thin rectangle centred on the line.
                #[allow(clippy::float_cmp)]
                DrawCmd::Line {
                    from,
                    to,
                    color,
                    width,
                } => {
                    let r = if from.y == to.y {
                        Rect::new(
                            from.x.min(to.x),
                            from.y - width / 2.0,
                            (to.x - from.x).abs(),
                            width,
                        )
                    } else {
                        Rect::new(
                            from.x - width / 2.0,
                            from.y.min(to.y),
                            width,
                            (to.y - from.y).abs(),
                        )
                    };
                    s.append_color(&rgba(color), &grect(r));
                }
                DrawCmd::Text(t) => self.text(s, frame.str(t.text), &t, widget),
                DrawCmd::Icon {
                    icon,
                    rect,
                    size,
                    color,
                } => self.icon(s, icon, rect, size, color, widget),
                DrawCmd::PushClip(r) => {
                    s.push_clip(&grect(r));
                    let outer = clips.last().copied().unwrap_or(full);
                    clips.push(intersect(outer, r));
                }
                DrawCmd::PopClip => {
                    if clips.len() > 1 {
                        clips.pop();
                        s.pop();
                    }
                }
                // Paths were drawn above, in runs. Program icons are not loaded
                // on Linux yet, so an image draws nothing.
                DrawCmd::Polyline { .. }
                | DrawCmd::FillPolygon { .. }
                | DrawCmd::Band { .. }
                | DrawCmd::Graph { .. }
                | DrawCmd::Image { .. } => {}
            }
            i += 1;
        }
        // An unbalanced list must not unbalance the snapshot.
        while clips.len() > 1 {
            clips.pop();
            s.pop();
        }
        self.evict();
    }

    fn evict(&mut self) {
        if self.frame.is_multiple_of(LAYOUT_TTL_FRAMES) {
            let frame = self.frame;
            self.layouts
                .retain(|_, c| frame - c.last_used < LAYOUT_TTL_FRAMES);
        }
    }

    fn layout(&mut self, text: &str, t: &TextCmd, widget: &gtk::Widget) -> &Cached {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        t.style.hash(&mut h);
        t.ellipsis.hash(&mut h);
        if t.ellipsis {
            t.rect.w.to_bits().hash(&mut h);
        }
        let key = h.finish();
        let frame = self.frame;
        let ui_family = self.ui_family.clone();
        let c = self.layouts.entry(key).or_insert_with(|| {
            let layout = widget.create_pango_layout(Some(text));
            let mut font = pango::FontDescription::new();
            match t.style.family {
                FontFamily::Mono => font.set_family("Monospace"),
                _ => font.set_family(ui_family.as_deref().unwrap_or("Sans")),
            }
            font.set_absolute_size(f64::from(t.style.size) * f64::from(pango::SCALE));
            font.set_weight(match t.style.weight {
                FontWeight::Regular => pango::Weight::Normal,
                FontWeight::SemiBold => pango::Weight::Semibold,
                FontWeight::Bold => pango::Weight::Bold,
            });
            layout.set_font_description(Some(&font));
            layout.set_single_paragraph_mode(true);
            if t.style.tabular_numbers {
                let attrs = pango::AttrList::new();
                attrs.insert(pango::AttrFontFeatures::new("tnum 1"));
                layout.set_attributes(Some(&attrs));
            }
            if t.ellipsis && !t.field {
                layout.set_width((t.rect.w * pango::SCALE as f32) as i32);
                layout.set_ellipsize(pango::EllipsizeMode::End);
            }
            let (_, logical) = layout.extents();
            Cached {
                layout,
                w: logical.width() as f32 / pango::SCALE as f32,
                h: logical.height() as f32 / pango::SCALE as f32,
                last_used: frame,
            }
        });
        c.last_used = frame;
        c
    }

    fn text(&mut self, snap: &gtk::Snapshot, text: &str, cmd: &TextCmd, widget: &gtk::Widget) {
        if text.is_empty() && !cmd.caret {
            return;
        }
        let cached = self.layout(text, cmd, widget);
        let (width, height) = (cached.w, cached.h);
        let rect = cmd.rect;
        // A field scrolls so its end stays visible; anything else aligns.
        let (left, shift) = if cmd.field {
            let shift = (width - rect.w).max(0.0);
            (rect.x - shift, shift)
        } else {
            let left = match cmd.halign {
                HAlign::Left => rect.x,
                HAlign::Center => rect.x + (rect.w - width) / 2.0,
                HAlign::Right => rect.x + rect.w - width,
            };
            (left, 0.0)
        };
        let top = match cmd.valign {
            VAlign::Top => rect.y,
            VAlign::Middle => rect.y + (rect.h - height) / 2.0,
            VAlign::Bottom => rect.y + rect.h - height,
        };
        let color = rgba(cmd.color);
        snap.push_clip(&grect(rect));
        snap.save();
        snap.translate(&graphene::Point::new(left.round(), top.round()));
        snap.append_layout(&cached.layout, &color);
        snap.restore();
        if cmd.caret {
            let caret = Rect::new(
                (rect.x - shift + width).round(),
                top.round(),
                CARET_W,
                height,
            );
            snap.append_color(&color, &grect(caret));
        }
        snap.pop();
    }

    fn icon(
        &mut self,
        s: &gtk::Snapshot,
        icon: Icon,
        rect: Rect,
        size: f32,
        color: Color,
        widget: &gtk::Widget,
    ) {
        let scale = widget.scale_factor();
        let px = size.round().max(1.0) as u32;
        let paintable = self.icons.entry((icon, px, scale)).or_insert_with(|| {
            let theme = gtk::IconTheme::for_display(&widget.display());
            // `has_icon` is not to be trusted (GTK 4.6 says yes to names its
            // Adwaita lacks, then draws "image-missing"); look each name up and
            // keep the first that resolves to a real icon.
            icon_names(icon).iter().find_map(|name| {
                let p = theme.lookup_icon(
                    name,
                    &[],
                    i32::try_from(px).unwrap_or(i32::MAX),
                    scale,
                    gtk::TextDirection::Ltr,
                    gtk::IconLookupFlags::FORCE_SYMBOLIC,
                );
                let missing = p
                    .icon_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("image-missing"));
                (!missing).then_some(p)
            })
        });
        let Some(p) = paintable else {
            return;
        };
        let x = rect.x + (rect.w - size) / 2.0;
        let y = rect.y + (rect.h - size) / 2.0;
        s.save();
        s.translate(&graphene::Point::new(x.round(), y.round()));
        p.snapshot_symbolic(
            s.upcast_ref::<gdk::Snapshot>(),
            f64::from(size),
            f64::from(size),
            &[rgba(color)],
        );
        s.restore();
    }
}

/// The image the host drew for the run starting at command `i`, if there is one:
/// drawn, and where its run ends. Images for runs before `i` are passed over.
fn draw_raster_at(
    s: &gtk::Snapshot,
    rasters: &mut std::iter::Peekable<std::slice::Iter<'_, abi::Raster>>,
    i: usize,
) -> Option<usize> {
    while rasters.next_if(|r| (r.first as usize) < i).is_some() {}
    let r = rasters.next_if(|r| r.first as usize == i)?;
    draw_raster(s, r);
    Some((r.end as usize).max(i + 1))
}

/// An image the host drew, one pixel to a device pixel at its rect.
fn draw_raster(s: &gtk::Snapshot, r: &abi::Raster) {
    if r.pixels.is_null() || r.width == 0 || r.height == 0 {
        return;
    }
    let (Ok(w), Ok(h)) = (i32::try_from(r.width), i32::try_from(r.height)) else {
        return;
    };
    let stride = r.width as usize * 4;
    // SAFETY: the host's image, `height` rows of `width * 4` bytes, valid until
    // the next call into the host; copied into the texture's bytes here.
    let pixels = unsafe { std::slice::from_raw_parts(r.pixels, stride * r.height as usize) };
    let bytes = glib::Bytes::from(pixels);
    let texture = gdk::MemoryTexture::new(
        w,
        h,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        stride,
    );
    s.append_texture(&texture, &grect(r.rect));
}

/// What a run of path commands touches, strokes included.
fn path_bounds(frame: &Frame<'_>, cmds: &[DrawCmd]) -> Option<Rect> {
    let mut bounds = None;
    for cmd in cmds {
        match *cmd {
            DrawCmd::Line {
                from, to, width, ..
            } => bounds_of([&from, &to], width, &mut bounds),
            DrawCmd::Polyline { points, width, .. } | DrawCmd::Graph { points, width, .. } => {
                bounds_of(frame.points(points), width, &mut bounds);
            }
            DrawCmd::FillPolygon { points, .. } => {
                bounds_of(frame.points(points), 1.0, &mut bounds);
            }
            DrawCmd::Band { top, bottom, .. } => {
                bounds_of(frame.points(top), 1.0, &mut bounds);
                bounds_of(frame.points(bottom), 1.0, &mut bounds);
            }
            _ => {}
        }
    }
    bounds
}

/// A run of path commands as one Cairo node over what they touch.
fn draw_paths(s: &gtk::Snapshot, frame: &Frame<'_>, cmds: &[DrawCmd], clip: Rect) {
    let bounds = path_bounds(frame, cmds);
    let Some(b) = bounds else {
        return;
    };
    let area = intersect(b, clip);
    if area.w <= 0.0 || area.h <= 0.0 {
        return;
    }
    let cr = s.append_cairo(&grect(Rect::new(
        area.x.floor(),
        area.y.floor(),
        area.w.ceil() + 1.0,
        area.h.ceil() + 1.0,
    )));
    cr.set_line_join(gtk::cairo::LineJoin::Round);
    cr.set_line_cap(gtk::cairo::LineCap::Butt);
    let set = |c: Color| {
        cr.set_source_rgba(
            f64::from(c.r),
            f64::from(c.g),
            f64::from(c.b),
            f64::from(c.a),
        );
    };
    let trace = |pts: &[Point], start: bool| {
        for (i, p) in pts.iter().enumerate() {
            if i == 0 && start {
                cr.move_to(f64::from(p.x), f64::from(p.y));
            } else {
                cr.line_to(f64::from(p.x), f64::from(p.y));
            }
        }
    };
    for cmd in cmds {
        match *cmd {
            DrawCmd::Line {
                from,
                to,
                color,
                width,
            } => {
                set(color);
                cr.set_line_width(f64::from(width));
                trace(&[from, to], true);
                let _ = cr.stroke();
            }
            DrawCmd::Polyline {
                points,
                color,
                width,
            }
            | DrawCmd::Graph {
                points,
                color,
                width,
            } => {
                let pts = frame.points(points);
                if pts.len() < 2 {
                    continue;
                }
                set(color);
                cr.set_line_width(f64::from(width));
                trace(pts, true);
                let _ = cr.stroke();
            }
            DrawCmd::FillPolygon { points, color } => {
                let pts = frame.points(points);
                if pts.len() < 3 {
                    continue;
                }
                set(color);
                trace(pts, true);
                cr.close_path();
                let _ = cr.fill();
            }
            DrawCmd::Band { top, bottom, color } => {
                // Along the top, back along the bottom: the region between, as
                // Windows draws it.
                let (t, b) = (frame.points(top), frame.points(bottom));
                if t.len() < 2 || b.len() < 2 {
                    continue;
                }
                set(color);
                trace(t, true);
                for p in b.iter().rev() {
                    cr.line_to(f64::from(p.x), f64::from(p.y));
                }
                cr.close_path();
                let _ = cr.fill();
            }
            _ => {}
        }
    }
}
