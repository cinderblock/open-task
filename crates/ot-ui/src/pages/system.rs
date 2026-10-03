//! The System page: the machine and its operating system in one place, as
//! TMOG's System Info view: Windows edition and build, the computer, its board
//! and firmware, the processor, the memory modules, the volumes, the page files,
//! and when it booted.
//!
//! The facts that never change are read once, on demand ([`Query::System`]); the
//! rest (uptime, memory in use, free space) come from the snapshot. "Copy all"
//! puts the whole page on the clipboard as text, for a bug report.

use std::fmt::Write as _;
use std::sync::Arc;

use ot_core::Snapshot;
use ot_model::system::SystemFacts;
use ot_paint::{DisplayList, HAlign, Rect, VAlign};

use crate::format;
use crate::theme::Theme;
use crate::view::{Effect, MouseButton, Query, Reaction, UiEvent};

const LABEL_W: f32 = 190.0;
const LINE_H: f32 = 22.0;
const SECTION_H: f32 = 34.0;
const BUTTON_W: f32 = 90.0;
const BUTTON_H: f32 = 28.0;

/// One line of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Section(&'static str),
    Fact(String, String),
}

#[derive(Debug)]
pub(crate) struct SystemPage {
    facts: Option<SystemFacts>,
    snap: Arc<Snapshot>,
    lines: Vec<Line>,
    scroll: f32,
    /// Where the Copy button was painted, and whether the pointer is on it.
    button: Rect,
    hover_button: bool,
    view: Rect,
}

impl Default for SystemPage {
    fn default() -> Self {
        let mut page = Self {
            facts: None,
            snap: Arc::new(Snapshot::default()),
            lines: Vec::new(),
            scroll: 0.0,
            button: Rect::ZERO,
            hover_button: false,
            view: Rect::ZERO,
        };
        page.rebuild();
        page
    }
}

impl SystemPage {
    #[must_use]
    pub fn query(&self) -> Option<Query> {
        self.facts.is_none().then_some(Query::System)
    }

    pub fn set_facts(&mut self, facts: SystemFacts) {
        self.facts = Some(facts);
        self.rebuild();
    }

    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) {
        self.snap = snap;
        self.rebuild();
    }

    fn fact(&mut self, label: &str, value: Option<impl AsRef<str>>) {
        if let Some(v) = value {
            let v = v.as_ref();
            if !v.is_empty() {
                self.lines.push(Line::Fact(label.to_owned(), v.to_owned()));
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn rebuild(&mut self) {
        self.lines.clear();
        let snap = Arc::clone(&self.snap);
        let hw = Arc::clone(&snap.hardware);
        let facts = self.facts.clone();
        let mut b = String::new();

        self.lines.push(Line::Section("Windows"));
        if let Some(f) = &facts {
            self.fact("Edition", f.os_name.as_deref());
            self.fact("Version", f.os_version.as_deref());
            self.fact("Build", f.os_build.as_deref());
            if let Some(ms) = f.os_installed_unix_ms {
                self.fact("Installed", Some(days_ago(ms, snapshot_ms(&snap))));
            }
            self.fact("Computer name", f.computer_name.as_deref());
        } else {
            self.fact("Edition", Some("Reading\u{2026}"));
        }
        if let Some(boot) = hw.boot_unix_ms {
            let up = (snapshot_ms(&snap) - boot).max(0) / 1000;
            format::uptime(&mut b, up as u64);
            self.fact("Up time", Some(b.as_str()));
        }

        self.lines.push(Line::Section("Computer"));
        if let Some(f) = &facts {
            self.fact("Manufacturer", f.system_manufacturer.as_deref());
            self.fact("Model", f.system_model.as_deref());
            self.fact(
                "Motherboard",
                join(f.board_manufacturer.as_deref(), f.board_product.as_deref()),
            );
            self.fact(
                "BIOS",
                join(f.bios_vendor.as_deref(), f.bios_version.as_deref()),
            );
            self.fact("BIOS date", f.bios_date.as_deref());
            self.fact(
                "Firmware",
                f.uefi.map(|u| if u { "UEFI" } else { "Legacy BIOS" }),
            );
            self.fact(
                "Secure Boot",
                f.secure_boot.map(|s| if s { "On" } else { "Off" }),
            );
        }

        self.lines.push(Line::Section("Processor"));
        self.fact("Name", hw.cpu_name.as_deref());
        if hw.logical_processors > 0 {
            b.clear();
            let _ = write!(
                b,
                "{} socket{}, {} core{}, {} logical processor{}",
                hw.sockets,
                plural(hw.sockets),
                hw.physical_cores,
                plural(hw.physical_cores),
                hw.logical_processors,
                plural(hw.logical_processors)
            );
            self.fact("Topology", Some(b.as_str()));
        }
        if let Some(f) = hw.base_frequency {
            format::clock(&mut b, f);
            self.fact("Base speed", Some(b.as_str()));
        }
        for (label, size) in [
            ("L1 cache", hw.cache_l1),
            ("L2 cache", hw.cache_l2),
            ("L3 cache", hw.cache_l3),
        ] {
            if let Some(s) = size {
                format::bytes(&mut b, s);
                self.fact(label, Some(b.as_str()));
            }
        }
        self.fact(
            "Virtualization",
            hw.virtualization
                .map(|v| if v { "Enabled" } else { "Disabled" }),
        );
        self.fact(
            "Hypervisor",
            hw.hypervisor.map(|h| {
                if h {
                    "Running (Hyper-V, or virtualization-based security)"
                } else {
                    "None"
                }
            }),
        );

        self.lines.push(Line::Section("Memory"));
        if let Some(installed) = hw.installed_memory {
            format::bytes(&mut b, installed);
            self.fact("Installed", Some(b.as_str()));
            let reserved = installed.saturating_sub(snap.memory.total);
            if snap.memory.total.get() > 0 && reserved.get() > 0 {
                format::bytes(&mut b, reserved);
                self.fact("Hardware reserved", Some(b.as_str()));
            }
        }
        if snap.memory.total.get() > 0 {
            format::bytes_of(&mut b, snap.memory.in_use(), snap.memory.total);
            self.fact("In use", Some(b.as_str()));
        }
        if let Some(s) = hw.memory_speed_mts {
            b.clear();
            let _ = write!(b, "{s} MT/s");
            self.fact("Speed", Some(b.as_str()));
        }
        if let (Some(used), Some(slots)) = (hw.memory_slots_used, hw.memory_slots) {
            b.clear();
            let _ = write!(b, "{used} of {slots}");
            self.fact("Slots used", Some(b.as_str()));
        }
        self.fact("Form factor", hw.memory_form_factor.as_deref());
        if let Some(f) = &facts {
            for d in &f.memory_devices {
                let Some(size) = d.size else {
                    self.fact(&d.slot, Some("Empty"));
                    continue;
                };
                b.clear();
                let mut v = String::new();
                format::bytes(&mut v, size);
                if let Some(k) = &d.kind {
                    let _ = write!(v, " {k}");
                }
                if let Some(s) = d.speed_mts {
                    let _ = write!(v, " at {s} MT/s");
                }
                if let Some(m) = &d.manufacturer {
                    let _ = write!(v, ", {m}");
                }
                if let Some(p) = &d.part_number {
                    let _ = write!(v, " {p}");
                }
                self.fact(&d.slot, Some(v));
            }
        }

        if !snap.volumes.is_empty() {
            self.lines.push(Line::Section("Storage"));
            for v in &snap.volumes {
                let mut text = String::new();
                if let Some(l) = &v.label {
                    let _ = write!(text, "{l}, ");
                }
                if let Some(fs) = &v.filesystem {
                    let _ = write!(text, "{fs}, ");
                }
                let mut free = String::new();
                format::bytes(&mut free, v.free);
                let mut total = String::new();
                format::bytes(&mut total, v.total);
                let _ = write!(text, "{free} free of {total}");
                if let Some(d) = v.disk {
                    let _ = write!(text, ", disk {d}");
                }
                if v.system {
                    text.push_str(", system");
                }
                if v.page_file {
                    text.push_str(", page file");
                }
                self.fact(&v.mount, Some(text));
            }
        }
        if let Some(f) = &facts {
            if !f.page_files.is_empty() {
                self.lines.push(Line::Section("Page files"));
                for p in &f.page_files {
                    self.lines.push(Line::Fact(String::new(), p.clone()));
                }
            }
        }
    }

    /// The whole page as text, for the clipboard.
    fn text(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Section(s) => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    let _ = writeln!(out, "{s}");
                }
                Line::Fact(label, value) => {
                    let _ = writeln!(out, "{label}: {value}");
                }
            }
        }
        out
    }

    fn height(&self) -> f32 {
        self.lines
            .iter()
            .map(|l| match l {
                Line::Section(_) => SECTION_H,
                Line::Fact(..) => LINE_H,
            })
            .sum()
    }

    pub fn handle(&mut self, ev: UiEvent) -> Reaction {
        match ev {
            UiEvent::MouseMove(p) => {
                let on = self.button.contains(p);
                let changed = on != self.hover_button;
                self.hover_button = on;
                Reaction::painted(changed)
            }
            UiEvent::MouseLeave => Reaction::painted(std::mem::take(&mut self.hover_button)),
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } if self.button.contains(at) => Reaction::effect(Effect::CopyText(self.text())),
            UiEvent::Wheel {
                lines, horizontal, ..
            } if !horizontal => {
                let max = (self.height() - self.view.h).max(0.0);
                let next = (self.scroll + lines * LINE_H * 3.0).clamp(0.0, max);
                let changed = (next - self.scroll).abs() > f32::EPSILON;
                self.scroll = next;
                Reaction::painted(changed)
            }
            UiEvent::Resize(_) => Reaction::REPAINT,
            _ => Reaction::NONE,
        }
    }

    pub fn paint(&mut self, dl: &mut DisplayList, rect: Rect, theme: &Theme) {
        let (title_line, body) = rect.split_top(theme.toolbar_h);
        dl.label("System", title_line, theme.title, theme.text);
        self.button = Rect::new(
            title_line.right() - BUTTON_W,
            title_line.y + (title_line.h - BUTTON_H) * 0.5,
            BUTTON_W,
            BUTTON_H,
        );
        let fill = if self.hover_button {
            theme.button_hover
        } else {
            theme.surface
        };
        dl.fill_round_rect(self.button, theme.card_radius, fill);
        dl.stroke_rect(self.button, theme.surface_border, 1.0);
        dl.text(
            "Copy all",
            self.button,
            theme.header,
            theme.text,
            HAlign::Center,
            VAlign::Middle,
            true,
        );

        let (_, body) = body.split_top(theme.gap * 0.5);
        dl.fill_round_rect(body, theme.card_radius, theme.surface);
        dl.stroke_rect(body, theme.surface_border, 1.0);
        let inner = body.inset(theme.pad * 2.0, theme.pad);
        self.view = inner;
        let max = (self.height() - inner.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        dl.push_clip(inner);
        let mut y = inner.y - self.scroll;
        for l in &self.lines {
            match l {
                Line::Section(s) => {
                    let r = Rect::new(inner.x, y, inner.w, SECTION_H);
                    if r.bottom() > inner.y && y < inner.bottom() {
                        dl.text(
                            s,
                            r,
                            theme.title,
                            theme.text,
                            HAlign::Left,
                            VAlign::Bottom,
                            false,
                        );
                        dl.fill_rect(
                            Rect::new(inner.x, r.bottom() - 1.0, inner.w, 1.0),
                            theme.grid,
                        );
                    }
                    y += SECTION_H;
                }
                Line::Fact(label, value) => {
                    let r = Rect::new(inner.x, y, inner.w, LINE_H);
                    if r.bottom() > inner.y && y < inner.bottom() {
                        let (l, v) = r.split_left(LABEL_W.min(r.w));
                        dl.label(label, l, theme.cell, theme.text_dim);
                        dl.label(value, v, theme.cell, theme.text);
                    }
                    y += LINE_H;
                }
            }
        }
        dl.pop_clip();
        if max > 0.0 {
            let track = Rect::new(inner.right() - 4.0, inner.y, 4.0, inner.h);
            let thumb_h = (track.h * inner.h / self.height()).max(20.0);
            let thumb_y = track.y + (track.h - thumb_h) * (self.scroll / max);
            dl.fill_round_rect(
                Rect::new(track.x, thumb_y, track.w, thumb_h),
                2.0,
                theme.scrollbar,
            );
        }
    }
}

fn plural(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn join(a: Option<&str>, b: Option<&str>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (Some(a), None) => Some(a.to_owned()),
        (None, Some(b)) => Some(b.to_owned()),
        (None, None) => None,
    }
}

fn snapshot_ms(snap: &Snapshot) -> i64 {
    snap.taken_at
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// `412 days ago`, from an epoch time and now.
fn days_ago(then_ms: i64, now_ms: i64) -> String {
    let days = (now_ms - then_ms).max(0) / 86_400_000;
    match days {
        0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        d => format!("{d} days ago"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_model::hardware::Hardware;
    use ot_model::system::MemoryDevice;
    use ot_model::Bytes;
    use ot_paint::{DrawCmd, Size};
    use std::time::{Duration, SystemTime};

    fn texts(page: &mut SystemPage) -> Vec<String> {
        let mut dl = DisplayList::new();
        page.paint(
            &mut dl,
            Rect::from_size(Size::new(900.0, 700.0)),
            &Theme::dark(),
        );
        assert_eq!(dl.clip_depth(), 0);
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text).to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn lists_the_facts_in_sections_and_copies_them() {
        let mut page = SystemPage::default();
        assert_eq!(page.query(), Some(Query::System));
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "Reading\u{2026}"), "{t:?}");
        page.set_snapshot(Arc::new(Snapshot {
            taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)),
            hardware: Arc::new(Hardware {
                cpu_name: Some("Test CPU".into()),
                sockets: 1,
                physical_cores: 6,
                logical_processors: 12,
                boot_unix_ms: Some(1_000_000_000 - 3_600_000),
                virtualization: Some(true),
                installed_memory: Some(Bytes(64 << 30)),
                memory_slots: Some(2),
                memory_slots_used: Some(2),
                ..Hardware::default()
            }),
            memory: ot_model::memory::MemorySample {
                total: Bytes(63 << 30),
                available: Bytes(30 << 30),
                ..Default::default()
            },
            ..Snapshot::default()
        }));
        page.set_facts(SystemFacts {
            os_name: Some("Windows 11 Pro".into()),
            os_build: Some("26100.1".into()),
            computer_name: Some("BOX".into()),
            memory_devices: vec![MemoryDevice {
                slot: "DIMM A1".into(),
                size: Some(Bytes(32 << 30)),
                speed_mts: Some(5600),
                kind: Some("DDR5".into()),
                ..MemoryDevice::default()
            }],
            page_files: vec!["C:\\pagefile.sys (8.0 GB)".into()],
            ..SystemFacts::default()
        });
        assert_eq!(page.query(), None);
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "Windows 11 Pro"), "{t:?}");
        assert!(
            t.iter()
                .any(|s| s == "1 socket, 6 cores, 12 logical processors"),
            "{t:?}"
        );
        assert!(t.iter().any(|s| s == "Enabled"), "{t:?}");
        assert!(t.iter().any(|s| s == "2 of 2"), "{t:?}");
        assert!(t.iter().any(|s| s == "32.0 GB DDR5 at 5600 MT/s"), "{t:?}");
        assert!(t.iter().any(|s| s == "1.00 GB"), "hardware reserved: {t:?}");
        assert!(t.iter().any(|s| s == "Page files"), "{t:?}");
        let text = page.text();
        assert!(text.contains("Build: 26100.1"), "{text}");
        let r = page.handle(UiEvent::MouseDown {
            at: page.button.center(),
            button: MouseButton::Left,
        });
        assert!(matches!(r.effect, Some(Effect::CopyText(_))));
        assert_eq!(days_ago(0, 2 * 86_400_000), "2 days ago");
    }
}
