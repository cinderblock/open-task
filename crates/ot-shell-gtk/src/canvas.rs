//! The window's one widget: it asks the host for a frame and draws it, and
//! reports input as the ABI's events.
//!
//! While the host says something is moving, a tick callback asks for a frame
//! per display refresh, and stops when the motion does; otherwise frames come
//! only when the host invalidates.

use std::cell::Cell;

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4 as gtk;
use ot_shell_abi::{self as abi, button, event, key, modifier};

use crate::plugin;

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use ot_shell_abi::{self as abi, event};

    use super::gtk;
    use crate::draw::{Frame, Painter};
    use crate::plugin;

    #[derive(Debug, Default)]
    pub struct Canvas {
        pub painter: RefCell<Painter>,
        /// The size last reported to the host, in DIPs.
        pub size: Cell<(i32, i32)>,
        /// A tick callback is asking for frames.
        pub ticking: Cell<bool>,
        /// Alternates each tick, to skip every other one in the background.
        pub odd_tick: Cell<bool>,
        /// With `OT_FRAME_STATS` set: what building the snapshots cost.
        pub stats: RefCell<crate::draw::DrawStats>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "OtCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {}

    impl WidgetImpl for Canvas {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if self.size.replace((width, height)) != (width, height) {
                // GTK's logical pixels are the ABI's DIPs; GSK applies the
                // display's scale.
                plugin::send(&abi::Event {
                    kind: event::RESIZE,
                    x: width as f32,
                    y: height as f32,
                    ..abi::Event::default()
                });
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(h) = plugin::host() else {
                return;
            };
            let widget = self.obj();
            let scale = super::scale(widget.upcast_ref());
            let mut list = abi::DisplayList::EMPTY;
            // SAFETY: the host's callback, on the UI thread; it fills `list`,
            // which stays valid until the next call into the host.
            unsafe { (h.paint)(h.ctx, scale, &raw mut list) };
            // SAFETY: as the ABI promises for a painted list.
            let frame = unsafe { Frame::from_abi(&list) };
            let drawing = std::time::Instant::now();
            self.painter
                .borrow_mut()
                .draw(snapshot, &frame, widget.upcast_ref());
            self.stats.borrow_mut().add(drawing.elapsed(), frame.len());
            // SAFETY: the host's callback, on the UI thread.
            if unsafe { (h.animating)(h.ctx) } && !self.ticking.replace(true) {
                widget.add_tick_callback(|w, _clock| {
                    let animating = plugin::host()
                        // SAFETY: as above.
                        .is_some_and(|h| unsafe { (h.animating)(h.ctx) });
                    if animating {
                        // In the background the window gets every other frame:
                        // still smooth, at half the cost, for a window glanced
                        // at rather than watched. The Windows shell does the same.
                        let imp = w.imp();
                        let odd = imp.odd_tick.replace(!imp.odd_tick.get());
                        let active = w
                            .root()
                            .and_downcast::<gtk::Window>()
                            .is_none_or(|win| win.is_active());
                        if active || !odd {
                            w.queue_draw();
                        }
                        glib::ControlFlow::Continue
                    } else {
                        w.imp().ticking.set(false);
                        glib::ControlFlow::Break
                    }
                });
            }
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

/// `gdk_surface_get_scale`, GTK 4.12's fractional scale, when this GTK has it.
type SurfaceScale = unsafe extern "C" fn(*mut gdk::ffi::GdkSurface) -> f64;

/// Device pixels a DIP for `widget`'s window: the surface's fractional scale
/// where GTK has it (4.12), else the integer scale factor.
fn scale(widget: &gtk::Widget) -> f32 {
    static GET_SCALE: std::sync::OnceLock<Option<SurfaceScale>> = std::sync::OnceLock::new();
    let get = GET_SCALE.get_or_init(|| {
        // SAFETY: looking a symbol up in what the process has loaded; a found
        // `gdk_surface_get_scale` has this signature.
        unsafe {
            let p = libc::dlsym(libc::RTLD_DEFAULT, c"gdk_surface_get_scale".as_ptr());
            (!p.is_null()).then(|| std::mem::transmute::<*mut libc::c_void, SurfaceScale>(p))
        }
    });
    let surface = widget.native().and_then(|n| n.surface());
    if let (Some(get), Some(surface)) = (get, surface) {
        // SAFETY: a live surface, for the call.
        let s = unsafe { get(surface.to_glib_none().0) } as f32;
        if s.is_finite() && s > 0.0 {
            return s;
        }
    }
    widget.scale_factor().max(1) as f32
}

/// The ABI's modifier bits from GDK's.
fn modifiers(state: gdk::ModifierType) -> u32 {
    let mut m = 0;
    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        m |= modifier::CTRL;
    }
    if state.contains(gdk::ModifierType::SHIFT_MASK) {
        m |= modifier::SHIFT;
    }
    if state.contains(gdk::ModifierType::ALT_MASK) {
        m |= modifier::ALT;
    }
    if state.contains(gdk::ModifierType::SUPER_MASK) {
        m |= modifier::SUPER;
    }
    m
}

/// A key press as the ABI's key and the text it types, if any.
fn key_event(keyval: gdk::Key, state: gdk::ModifierType) -> (Option<abi::Event>, Option<char>) {
    let mods = modifiers(state);
    let named = |k| {
        Some(abi::Event {
            kind: event::KEY,
            key: k,
            modifiers: mods,
            ..abi::Event::default()
        })
    };
    let k = match keyval {
        gdk::Key::Up | gdk::Key::KP_Up => named(key::UP),
        gdk::Key::Down | gdk::Key::KP_Down => named(key::DOWN),
        gdk::Key::Page_Up | gdk::Key::KP_Page_Up => named(key::PAGE_UP),
        gdk::Key::Page_Down | gdk::Key::KP_Page_Down => named(key::PAGE_DOWN),
        gdk::Key::Home | gdk::Key::KP_Home => named(key::HOME),
        gdk::Key::End | gdk::Key::KP_End => named(key::END),
        gdk::Key::Left | gdk::Key::KP_Left => named(key::LEFT),
        gdk::Key::Right | gdk::Key::KP_Right => named(key::RIGHT),
        gdk::Key::BackSpace => named(key::BACKSPACE),
        gdk::Key::Delete | gdk::Key::KP_Delete => named(key::DELETE),
        gdk::Key::Escape => named(key::ESCAPE),
        gdk::Key::Return | gdk::Key::KP_Enter => named(key::ENTER),
        gdk::Key::Tab | gdk::Key::ISO_Left_Tab => named(key::TAB),
        gdk::Key::Menu => named(key::MENU),
        gdk::Key::F10 if mods & modifier::SHIFT != 0 => named(key::MENU),
        _ => None,
    };
    if k.is_some() {
        return (k, None);
    }
    let shortcut = mods & (modifier::CTRL | modifier::ALT | modifier::SUPER) != 0;
    if shortcut {
        // A letter or digit with Ctrl: the key as labelled, lower case.
        let c = keyval
            .to_lower()
            .to_unicode()
            .filter(char::is_ascii_alphanumeric);
        return (
            c.map(|c| abi::Event {
                kind: event::KEY,
                key: key::CHARACTER,
                codepoint: u32::from(c.to_ascii_lowercase()),
                modifiers: mods,
                ..abi::Event::default()
            }),
            None,
        );
    }
    (None, keyval.to_unicode().filter(|c| !c.is_control()))
}

impl Canvas {
    #[must_use]
    pub fn new() -> Self {
        let c: Self = glib::Object::new();
        c.set_hexpand(true);
        c.set_vexpand(true);
        c.set_focusable(true);
        c.set_can_focus(true);
        c
    }

    /// The input controllers: pointer, wheel and keys. Keys go on the window,
    /// so they arrive whatever has focus.
    pub fn attach_input(&self, window: &gtk::Window) {
        self.attach_pointer();
        attach_keys(window);
    }

    fn attach_pointer(&self) {
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.connect_pressed(|g, _n, x, y| {
            let b = match g.current_button() {
                1 => button::LEFT,
                3 => button::RIGHT,
                2 => button::MIDDLE,
                _ => return,
            };
            if let Some(w) = g.widget() {
                w.grab_focus();
            }
            plugin::send(&abi::Event {
                kind: event::MOUSE_DOWN,
                button: b,
                modifiers: modifiers(g.current_event_state()),
                x: x as f32,
                y: y as f32,
                ..abi::Event::default()
            });
        });
        click.connect_released(|g, _n, x, y| {
            let (x, y) = (x as f32, y as f32);
            match g.current_button() {
                1 => plugin::send(&abi::Event {
                    kind: event::MOUSE_UP,
                    button: button::LEFT,
                    x,
                    y,
                    ..abi::Event::default()
                }),
                // A right click asks for the context menu once released, as on
                // Windows.
                3 => plugin::send(&abi::Event {
                    kind: event::CONTEXT_MENU,
                    flag: 1,
                    x,
                    y,
                    ..abi::Event::default()
                }),
                _ => {}
            }
        });
        self.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(|_, x, y| {
            plugin::send(&abi::Event {
                kind: event::MOUSE_MOVE,
                x: x as f32,
                y: y as f32,
                ..abi::Event::default()
            });
        });
        motion.connect_leave(|_| {
            plugin::send(&abi::Event {
                kind: event::MOUSE_LEAVE,
                ..abi::Event::default()
            });
        });
        self.add_controller(motion);

        // The wheel's position is the pointer's, which the motion controller
        // last saw.
        let pointer = std::rc::Rc::new(Cell::new((0.0f32, 0.0f32)));
        let at = pointer.clone();
        let track = gtk::EventControllerMotion::new();
        track.connect_motion(move |_, x, y| at.set((x as f32, y as f32)));
        self.add_controller(track);
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.connect_scroll(move |c, dx, dy| {
            let (x, y) = pointer.get();
            plugin::send(&abi::Event {
                kind: event::WHEEL,
                modifiers: modifiers(c.current_event_state()),
                x,
                y,
                dx: dx as f32,
                dy: dy as f32,
                ..abi::Event::default()
            });
            glib::Propagation::Stop
        });
        self.add_controller(scroll);
    }
}

fn attach_keys(window: &gtk::Window) {
    {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(|_, keyval, _code, state| {
            let (k, text) = key_event(keyval, state);
            if let Some(ev) = k {
                plugin::send(&ev);
                return glib::Propagation::Stop;
            }
            if let Some(c) = text {
                plugin::send(&abi::Event {
                    kind: event::TEXT,
                    codepoint: u32::from(c),
                    ..abi::Event::default()
                });
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(|_| {
            plugin::send(&abi::Event {
                kind: event::FOCUS,
                flag: 1,
                ..abi::Event::default()
            });
        });
        focus.connect_leave(|_| {
            plugin::send(&abi::Event {
                kind: event::FOCUS,
                flag: 0,
                ..abi::Event::default()
            });
        });
        window.add_controller(focus);
    }
}
