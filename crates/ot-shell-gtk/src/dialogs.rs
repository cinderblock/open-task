//! Menus and dialogs, shown without blocking: each answers through the host's
//! callback for it when the user is done.
//!
//! GTK 4.6 is the floor, so the dialogs are `GtkMessageDialog` and
//! `GtkFileChooserNative` (which goes through the desktop portal where there is
//! one), both deprecated in 4.10 in favour of APIs 4.6 lacks.
#![allow(deprecated)]

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use ot_shell_abi::{self as abi, menu as kind, notice};

use crate::plugin::{self, with_ui};

/// The window dialogs belong to.
fn parent() -> Option<gtk::Window> {
    with_ui(|ui| ui.window.clone())
}

/// A popup menu at `x`, `y`. Items are actions in a group of the popover's own;
/// a checked item's action has a boolean state, which GTK shows as a check mark.
pub fn menu(token: u64, x: f32, y: f32, items: &[abi::MenuItem]) {
    let Some(canvas) = with_ui(|ui| ui.canvas.clone()) else {
        return;
    };
    let group = gio::SimpleActionGroup::new();
    let answered = Rc::new(Cell::new(false));
    // The menu and section each open submenu interrupted: (menu, section, label).
    let mut stack: Vec<(gio::Menu, gio::Menu, String)> = Vec::new();
    let top = gio::Menu::new();
    let mut section = gio::Menu::new();
    let mut current_top = top.clone();
    let mut n = 0u32;
    for item in items {
        // SAFETY: the host passes each label with its length for the call.
        let label = unsafe { plugin::text(item.label, item.label_len) };
        match item.kind {
            kind::ITEM => {
                n += 1;
                let name = format!("i{n}");
                let action = if item.checked {
                    gio::SimpleAction::new_stateful(&name, None, &true.to_variant())
                } else {
                    gio::SimpleAction::new(&name, None)
                };
                action.set_enabled(item.enabled);
                let id = item.id;
                let done = answered.clone();
                action.connect_activate(move |_, _| {
                    done.set(true);
                    if let Some(h) = plugin::host() {
                        // SAFETY: the host's callback, on the UI thread.
                        unsafe { (h.menu_chosen)(h.ctx, token, true, id) };
                    }
                });
                group.add_action(&action);
                section.append(Some(&label), Some(&format!("ot.{name}")));
            }
            kind::SEPARATOR => {
                // GTK draws separators between sections.
                current_top.append_section(None, &section);
                section = gio::Menu::new();
            }
            kind::SUBMENU => {
                // The submenu's own menu and section; the outer ones wait on the
                // stack, the outer section still open.
                stack.push((current_top, section, label));
                current_top = gio::Menu::new();
                section = gio::Menu::new();
            }
            kind::SUBMENU_END => {
                current_top.append_section(None, &section);
                if let Some((outer_top, outer_section, label)) = stack.pop() {
                    outer_section.append_submenu(Some(&label), &current_top);
                    current_top = outer_top;
                    section = outer_section;
                } else {
                    section = gio::Menu::new();
                }
            }
            _ => {}
        }
    }
    current_top.append_section(None, &section);

    let popover = gtk::PopoverMenu::from_model(Some(&top));
    popover.insert_action_group("ot", Some(&group));
    popover.set_parent(&canvas);
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(
        x.round() as i32,
        y.round() as i32,
        1,
        1,
    )));
    popover.connect_closed(move |p| {
        // An item's action runs after the menu closes; look once it has.
        let done = answered.clone();
        let p = p.clone();
        glib::idle_add_local_once(move || {
            if !done.get() {
                if let Some(h) = plugin::host() {
                    // SAFETY: the host's callback, on the UI thread.
                    unsafe { (h.menu_chosen)(h.ctx, token, false, 0) };
                }
            }
            p.unparent();
        });
    });
    popover.popup();
}

/// The first paragraph as the dialog's heading, the rest as its body: the
/// host's messages are written "What happened.\n\nWhy."
fn split(text: &str) -> (&str, Option<&str>) {
    match text.split_once("\n\n") {
        Some((head, body)) => (head, Some(body)),
        None => (text, None),
    }
}

fn dialog(kind: gtk::MessageType, buttons: gtk::ButtonsType, text: &str) -> gtk::MessageDialog {
    let (head, body) = split(text);
    let d = gtk::MessageDialog::new(
        parent().as_ref(),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        kind,
        buttons,
        head,
    );
    d.set_secondary_text(body);
    d.set_title(Some("open-task"));
    d
}

pub fn message(kind: u32, text: &str) {
    let kind = match kind {
        notice::INFO => gtk::MessageType::Info,
        notice::ERROR => gtk::MessageType::Error,
        _ => gtk::MessageType::Warning,
    };
    let d = dialog(kind, gtk::ButtonsType::Ok, text);
    d.connect_response(|d, _| d.destroy());
    d.present();
}

/// Yes or No, with No the default, as the Windows build asks.
pub fn confirm(token: u64, question: &str) {
    let d = dialog(gtk::MessageType::Warning, gtk::ButtonsType::None, question);
    d.add_button("_No", gtk::ResponseType::No);
    d.add_button("_Yes", gtk::ResponseType::Yes);
    d.set_default_response(gtk::ResponseType::No);
    d.connect_response(move |d, r| {
        d.destroy();
        if let Some(h) = plugin::host() {
            // SAFETY: the host's callback, on the UI thread.
            unsafe { (h.confirmed)(h.ctx, token, r == gtk::ResponseType::Yes) };
        }
    });
    d.present();
}

pub fn save_file(token: u64, title: &str, name: &str, filter_name: &str, pattern: &str) {
    let chooser = gtk::FileChooserNative::new(
        Some(title),
        parent().as_ref(),
        gtk::FileChooserAction::Save,
        Some("_Save"),
        Some("_Cancel"),
    );
    chooser.set_modal(true);
    chooser.set_current_name(name);
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(filter_name));
    filter.add_pattern(pattern);
    chooser.add_filter(&filter);
    // The chooser must outlive this function; the response handler holds it
    // until the user answers.
    let keep = Rc::new(std::cell::RefCell::new(Some(chooser.clone())));
    chooser.connect_response(move |c, r| {
        let path = (r == gtk::ResponseType::Accept)
            .then(|| c.file().and_then(|f| f.path()))
            .flatten();
        if let Some(h) = plugin::host() {
            match path {
                Some(p) => {
                    let s = p.to_string_lossy();
                    // SAFETY: the host's callback; the path outlives the call.
                    unsafe { (h.file_chosen)(h.ctx, token, s.as_ptr().cast(), s.len()) };
                }
                // SAFETY: as above; NULL means cancelled.
                None => unsafe { (h.file_chosen)(h.ctx, token, std::ptr::null(), 0) },
            }
        }
        keep.borrow_mut().take();
    });
    chooser.show();
}

/// The Run dialog: a command line, and whether to run it as administrator.
pub fn run(token: u64, initial: &str) {
    let window = gtk::Window::builder()
        .title("Run new task")
        .modal(true)
        .resizable(false)
        .default_width(440)
        .build();
    if let Some(p) = parent() {
        window.set_transient_for(Some(&p));
    }
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    for side in [
        gtk::Widget::set_margin_top,
        gtk::Widget::set_margin_bottom,
        gtk::Widget::set_margin_start,
        gtk::Widget::set_margin_end,
    ] {
        side(content.upcast_ref(), 18);
    }
    let prompt = gtk::Label::new(Some(
        "Type the name of a program, folder, document or internet resource, and \
         open-task will open it for you.",
    ));
    prompt.set_wrap(true);
    prompt.set_xalign(0.0);
    let entry = gtk::Entry::new();
    entry.set_text(initial);
    entry.set_activates_default(true);
    let elevated = gtk::CheckButton::with_label("Create this task with administrative privileges");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    content.append(&prompt);
    content.append(&entry);
    content.append(&elevated);
    content.append(&buttons);
    window.set_child(Some(&content));
    window.set_default_widget(Some(&ok));

    let answered = Rc::new(Cell::new(false));
    let answer = {
        let answered = answered.clone();
        move |command: Option<String>, elevated: bool| {
            if answered.replace(true) {
                return;
            }
            if let Some(h) = plugin::host() {
                match command {
                    Some(c) if !c.trim().is_empty() => {
                        // SAFETY: the host's callback; the text outlives the call.
                        unsafe {
                            (h.run_entered)(h.ctx, token, c.as_ptr().cast(), c.len(), elevated);
                        };
                    }
                    // SAFETY: as above; NULL means cancelled.
                    _ => unsafe { (h.run_entered)(h.ctx, token, std::ptr::null(), 0, false) },
                }
            }
        }
    };
    {
        let (window, entry, elevated, answer) = (
            window.clone(),
            entry.clone(),
            elevated.clone(),
            answer.clone(),
        );
        ok.connect_clicked(move |_| {
            answer(Some(entry.text().to_string()), elevated.is_active());
            window.destroy();
        });
    }
    {
        let (window, answer) = (window.clone(), answer.clone());
        cancel.connect_clicked(move |_| {
            answer(None, false);
            window.destroy();
        });
    }
    // Escape cancels, as in any dialog; a plain window does not do it alone.
    let keys = gtk::EventControllerKey::new();
    {
        let window = window.clone();
        keys.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(keys);
    window.connect_close_request(move |_| {
        answer(None, false);
        glib::Propagation::Proceed
    });
    window.present();
    entry.grab_focus();
}
