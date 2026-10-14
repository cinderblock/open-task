//! Keys as a plugin reports them, to the view's events. The bindings are the
//! Windows shell's: Task Manager's and Process Explorer's.

use ot_shell_abi::{key, modifier, Event};
use ot_ui::{Command, Key, MenuAction, Page, UiEvent, ViewMode};

/// What a key press means, if anything. Escape with the crosshair out is the
/// host's, not the view's, and is decided before this.
#[must_use]
pub fn map(ev: &Event) -> Option<UiEvent> {
    let ctrl = ev.modifiers & modifier::CTRL != 0;
    let shift = ev.modifiers & modifier::SHIFT != 0;
    Some(match ev.key {
        key::UP => UiEvent::Key(Key::Up),
        key::DOWN => UiEvent::Key(Key::Down),
        key::PAGE_UP => UiEvent::Key(Key::PageUp),
        key::PAGE_DOWN => UiEvent::Key(Key::PageDown),
        key::HOME => UiEvent::Key(Key::Home),
        key::END => UiEvent::Key(Key::End),
        key::LEFT => UiEvent::Key(Key::Left),
        key::RIGHT => UiEvent::Key(Key::Right),
        key::BACKSPACE if ctrl => UiEvent::Key(Key::WordBackspace),
        key::BACKSPACE => UiEvent::Key(Key::Backspace),
        key::ESCAPE => UiEvent::Key(Key::Escape),
        key::ENTER => UiEvent::Key(Key::Enter),
        // Delete ends the selected process; Shift+Delete its whole tree.
        key::DELETE if shift => UiEvent::Command(Command::Menu(MenuAction::EndTree)),
        key::DELETE => UiEvent::Command(Command::Menu(MenuAction::EndTask)),
        // Ctrl+Tab and Ctrl+Shift+Tab walk the pages.
        key::TAB if ctrl => UiEvent::Command(Command::StepPage(if shift { -1 } else { 1 })),
        // The menu key, or Shift+F10 as the plugin reports it.
        key::MENU => UiEvent::ContextMenu { at: None },
        key::CHARACTER if ctrl => match char::from_u32(ev.codepoint)? {
            't' => UiEvent::Command(Command::ToggleView),
            'm' => UiEvent::Command(Command::SetView(ViewMode::Map)),
            'h' => UiEvent::Command(Command::SetView(ViewMode::History)),
            'f' => UiEvent::Command(Command::Find),
            'n' => UiEvent::Command(Command::RunTask),
            c @ '1'..='9' => {
                let n = c.to_digit(10)? as usize;
                UiEvent::Command(Command::SetPage(Page::nth(n)?))
            }
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(k: u32, codepoint: char, modifiers: u32) -> Option<UiEvent> {
        map(&Event {
            kind: ot_shell_abi::event::KEY,
            key: k,
            codepoint: codepoint as u32,
            modifiers,
            ..Event::default()
        })
    }

    #[test]
    fn the_windows_bindings_hold() {
        assert_eq!(
            press(key::CHARACTER, 't', modifier::CTRL),
            Some(UiEvent::Command(Command::ToggleView))
        );
        assert_eq!(
            press(key::DELETE, '\0', modifier::SHIFT),
            Some(UiEvent::Command(Command::Menu(MenuAction::EndTree)))
        );
        assert_eq!(
            press(key::TAB, '\0', modifier::CTRL | modifier::SHIFT),
            Some(UiEvent::Command(Command::StepPage(-1)))
        );
        assert_eq!(
            press(key::CHARACTER, '2', modifier::CTRL),
            Page::nth(2).map(|p| UiEvent::Command(Command::SetPage(p)))
        );
        // A letter without Ctrl is text, which comes as a TEXT event instead.
        assert_eq!(press(key::CHARACTER, 't', 0), None);
    }
}
