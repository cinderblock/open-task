//! The replay transport: what the shell shows across the bottom of the window
//! while a recording plays instead of the live sampler.
//!
//! The bar has the recording's name, step-back, play/pause and step-forward
//! buttons, a slider over the frames, the position and length as times, and a
//! speed button that cycles through [`SPEEDS`]. It owns no player: every press is
//! an [`Effect::Replay`] the shell applies to its [`ot_core::Player`], and the
//! player's next publish brings a fresh [`ReplayState`] back through
//! [`crate::App::set_replay`]. Dragging the slider seeks as the pointer moves.

use ot_paint::{Color, DisplayList, HAlign, Icon, Point, Rect, VAlign};

use crate::format;
use crate::theme::Theme;
use crate::view::{Effect, MouseButton, Reaction, UiEvent};

/// Where a replay stands, as the shell reports it after each frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReplayState {
    /// The file's name, without its directory.
    pub name: String,
    /// The frame on show, `0..len`.
    pub position: usize,
    pub len: usize,
    pub playing: bool,
    pub speed: f32,
    /// Time into the recording at `position`, and the recording's length.
    pub elapsed_ms: u64,
    pub duration_ms: u64,
}

/// What the transport asks the shell to do with the player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReplayAction {
    /// Play if paused, pause if playing.
    Toggle,
    /// Go to this frame.
    Seek(usize),
    /// Move this many frames, negative for back.
    Step(i64),
    /// Play this many times faster than real time.
    Speed(f32),
}

/// What is being recorded right now, for the Settings page's card.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordingView {
    /// The file's name, without its directory.
    pub name: String,
    pub frames: u64,
    pub bytes: u64,
    /// Frames the writer could not keep up with.
    pub dropped: u64,
}

/// The speeds the speed button cycles through.
pub const SPEEDS: [f32; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];

/// The next speed after `speed`, wrapping.
#[must_use]
pub fn next_speed(speed: f32) -> f32 {
    let i = SPEEDS
        .iter()
        .position(|&s| (s - speed).abs() < 0.01)
        .map_or(1, |i| (i + 1) % SPEEDS.len());
    SPEEDS[i]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Back,
    Play,
    Forward,
    Slider,
    Speed,
}

/// The bar's state and where its parts were painted.
#[derive(Debug, Default)]
pub(crate) struct Transport {
    pub state: ReplayState,
    bar: Rect,
    back: Rect,
    play: Rect,
    forward: Rect,
    slider: Rect,
    speed: Rect,
    hover: Option<Part>,
    /// The slider is being dragged: moves seek until the button goes up.
    dragging: bool,
}

const BUTTON: f32 = 28.0;
const NAME_W: f32 = 200.0;
const TIME_W: f32 = 132.0;
const SPEED_W: f32 = 48.0;
const TRACK_H: f32 = 4.0;
const KNOB: f32 = 12.0;

impl Transport {
    /// The bar's height, taken off the bottom of the window.
    pub const H: f32 = 44.0;

    pub fn new(state: ReplayState) -> Self {
        Self {
            state,
            ..Self::default()
        }
    }

    pub fn set(&mut self, state: ReplayState) {
        self.state = state;
    }

    fn part_at(&self, p: Point) -> Option<Part> {
        [
            (self.back, Part::Back),
            (self.play, Part::Play),
            (self.forward, Part::Forward),
            (self.slider, Part::Slider),
            (self.speed, Part::Speed),
        ]
        .into_iter()
        .find(|(r, _)| r.contains(p))
        .map(|(_, part)| part)
    }

    /// The frame under `x` on the slider.
    fn frame_at(&self, x: f32) -> usize {
        if self.state.len == 0 || self.slider.w <= 0.0 {
            return 0;
        }
        let t = ((x - self.slider.x) / self.slider.w).clamp(0.0, 1.0);
        (t * (self.state.len - 1) as f32).round() as usize
    }

    /// Input the bar takes for itself: `Some` when handled (even if nothing
    /// changed), `None` when the event is not the bar's.
    pub fn handle(&mut self, ev: UiEvent) -> Option<Reaction> {
        match ev {
            UiEvent::MouseMove(p) => {
                if self.dragging {
                    return Some(Reaction::effect(Effect::Replay(ReplayAction::Seek(
                        self.frame_at(p.x),
                    ))));
                }
                let hover = self.part_at(p);
                if hover != self.hover {
                    self.hover = hover;
                    return Some(Reaction::REPAINT);
                }
                self.bar.contains(p).then_some(Reaction::NONE)
            }
            UiEvent::MouseLeave => {
                self.dragging = false;
                if self.hover.take().is_some() {
                    return Some(Reaction::REPAINT);
                }
                None
            }
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => {
                let action = match self.part_at(at) {
                    Some(Part::Play) => ReplayAction::Toggle,
                    Some(Part::Back) => ReplayAction::Step(-1),
                    Some(Part::Forward) => ReplayAction::Step(1),
                    Some(Part::Speed) => ReplayAction::Speed(next_speed(self.state.speed)),
                    Some(Part::Slider) => {
                        self.dragging = true;
                        ReplayAction::Seek(self.frame_at(at.x))
                    }
                    None => return self.bar.contains(at).then_some(Reaction::NONE),
                };
                Some(Reaction::effect(Effect::Replay(action)))
            }
            UiEvent::MouseUp { .. } if self.dragging => {
                self.dragging = false;
                Some(Reaction::NONE)
            }
            UiEvent::Wheel { at, lines, .. } if self.bar.contains(at) => {
                // Wheel toward the user steps forward, as scrolling down does.
                Some(Reaction::effect(Effect::Replay(ReplayAction::Step(
                    lines.round() as i64,
                ))))
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn paint(&mut self, dl: &mut DisplayList, area: Rect, theme: &Theme, buf: &mut String) {
        self.bar = area;
        dl.fill_rect(area, theme.surface);
        dl.fill_rect(Rect::new(area.x, area.y, area.w, 1.0), theme.surface_border);
        let inner = area.inset(theme.pad, (area.h - BUTTON) * 0.5);

        let (name, rest) = inner.split_left(NAME_W.min(inner.w));
        buf.clear();
        buf.push_str("Replaying ");
        buf.push_str(&self.state.name);
        dl.label(buf, name, theme.small, theme.text_dim);

        let (back, rest) = rest.split_left(BUTTON);
        let (_, rest) = rest.split_left(theme.pad * 0.5);
        let (play, rest) = rest.split_left(BUTTON);
        let (_, rest) = rest.split_left(theme.pad * 0.5);
        let (forward, rest) = rest.split_left(BUTTON);
        self.back = back;
        self.play = play;
        self.forward = forward;
        let last = self.state.len.saturating_sub(1);
        self.button(
            dl,
            back,
            Icon::Previous,
            Part::Back,
            self.state.position > 0,
            theme,
        );
        let play_icon = if self.state.playing {
            Icon::Pause
        } else {
            Icon::Play
        };
        self.button(dl, play, play_icon, Part::Play, self.state.len > 1, theme);
        self.button(
            dl,
            forward,
            Icon::Next,
            Part::Forward,
            self.state.position < last,
            theme,
        );

        let (rest, speed) = rest.split_left((rest.w - SPEED_W).max(0.0));
        self.speed = speed;
        buf.clear();
        if (self.state.speed - self.state.speed.round()).abs() < 0.01 {
            let _ = std::fmt::Write::write_fmt(buf, format_args!("{:.0}\u{d7}", self.state.speed));
        } else {
            let _ = std::fmt::Write::write_fmt(buf, format_args!("{:.1}\u{d7}", self.state.speed));
        }
        let speed_fill = if self.hover == Some(Part::Speed) {
            theme.button_hover
        } else {
            Color::TRANSPARENT
        };
        dl.fill_round_rect(speed, theme.card_radius, speed_fill);
        dl.text(
            buf,
            speed,
            theme.cell,
            theme.text,
            HAlign::Center,
            VAlign::Middle,
            false,
        );

        let (rest, time) = rest.split_left((rest.w - TIME_W).max(0.0));
        let mut total = String::new();
        format::hms(&mut total, self.state.duration_ms / 1000);
        format::hms(buf, self.state.elapsed_ms / 1000);
        buf.push_str(" / ");
        buf.push_str(&total);
        dl.text(
            buf,
            time,
            theme.cell_num,
            theme.text,
            HAlign::Right,
            VAlign::Middle,
            false,
        );

        let slider = rest.inset(theme.pad, 0.0);
        self.slider = slider;
        if slider.w > 0.0 {
            let track = Rect::new(
                slider.x,
                slider.y + (slider.h - TRACK_H) * 0.5,
                slider.w,
                TRACK_H,
            );
            dl.fill_round_rect(track, TRACK_H * 0.5, theme.grid);
            let t = if last == 0 {
                0.0
            } else {
                self.state.position as f32 / last as f32
            };
            let done = Rect::new(track.x, track.y, track.w * t, track.h);
            dl.fill_round_rect(done, TRACK_H * 0.5, theme.accent);
            let knob = Rect::new(
                (track.x + track.w * t - KNOB * 0.5).clamp(track.x, track.right() - KNOB),
                slider.y + (slider.h - KNOB) * 0.5,
                KNOB,
                KNOB,
            );
            dl.fill_round_rect(knob, KNOB * 0.5, theme.accent);
        }
    }

    fn button(
        &self,
        dl: &mut DisplayList,
        r: Rect,
        icon: Icon,
        part: Part,
        enabled: bool,
        theme: &Theme,
    ) {
        if enabled && self.hover == Some(part) {
            dl.fill_round_rect(r, theme.card_radius, theme.button_hover);
        }
        let color = if enabled { theme.text } else { theme.text_dim };
        dl.icon(icon, r, 14.0, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_paint::DrawCmd;

    fn state() -> ReplayState {
        ReplayState {
            name: "run.otrec".to_owned(),
            position: 25,
            len: 101,
            playing: false,
            speed: 1.0,
            elapsed_ms: 25_000,
            duration_ms: 100_000,
        }
    }

    fn painted(t: &mut Transport) -> (DisplayList, Vec<String>) {
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        t.paint(
            &mut dl,
            Rect::new(0.0, 600.0, 1000.0, Transport::H),
            &Theme::dark(),
            &mut buf,
        );
        let texts = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text).to_owned()),
                _ => None,
            })
            .collect();
        (dl, texts)
    }

    #[test]
    fn the_bar_names_the_file_and_the_times_and_its_buttons_act() {
        let mut t = Transport::new(state());
        let (dl, texts) = painted(&mut t);
        assert!(
            texts.contains(&"Replaying run.otrec".to_owned()),
            "{texts:?}"
        );
        assert!(texts.contains(&"0:00:25 / 0:01:40".to_owned()), "{texts:?}");
        assert!(texts.contains(&"1\u{d7}".to_owned()), "{texts:?}");
        let icons: Vec<Icon> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Icon { icon, .. } => Some(*icon),
                _ => None,
            })
            .collect();
        assert_eq!(icons, [Icon::Previous, Icon::Play, Icon::Next]);

        let (play_at, back_at, forward_at, speed_at) = (
            t.play.center(),
            t.back.center(),
            t.forward.center(),
            t.speed.center(),
        );
        let click = |t: &mut Transport, at: Point| {
            t.handle(UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            })
            .and_then(|r| r.effect)
        };
        assert_eq!(
            click(&mut t, play_at),
            Some(Effect::Replay(ReplayAction::Toggle))
        );
        assert_eq!(
            click(&mut t, back_at),
            Some(Effect::Replay(ReplayAction::Step(-1)))
        );
        assert_eq!(
            click(&mut t, forward_at),
            Some(Effect::Replay(ReplayAction::Step(1)))
        );
        assert_eq!(
            click(&mut t, speed_at),
            Some(Effect::Replay(ReplayAction::Speed(2.0)))
        );
        // The slider seeks where it is clicked, then follows a drag.
        let slider = t.slider;
        let middle = Point::new(slider.x + slider.w * 0.5, slider.center().y);
        assert_eq!(
            click(&mut t, middle),
            Some(Effect::Replay(ReplayAction::Seek(50)))
        );
        let r = t.handle(UiEvent::MouseMove(Point::new(slider.right(), middle.y)));
        assert_eq!(
            r.and_then(|r| r.effect),
            Some(Effect::Replay(ReplayAction::Seek(100)))
        );
        assert!(t
            .handle(UiEvent::MouseUp {
                at: middle,
                button: MouseButton::Left
            })
            .is_some());
        // Off the bar, events are not the bar's.
        assert!(t
            .handle(UiEvent::MouseMove(Point::new(10.0, 10.0)))
            .is_none());

        t.set(ReplayState {
            playing: true,
            ..state()
        });
        let (dl, _) = painted(&mut t);
        assert!(dl.cmds().iter().any(|c| matches!(
            c,
            DrawCmd::Icon {
                icon: Icon::Pause,
                ..
            }
        )));
    }

    #[test]
    fn speeds_cycle_and_wrap() {
        assert_eq!(next_speed(1.0), 2.0);
        assert_eq!(next_speed(8.0), 0.5);
        assert_eq!(next_speed(3.0), 1.0, "an unknown speed goes to 1x");
    }
}
