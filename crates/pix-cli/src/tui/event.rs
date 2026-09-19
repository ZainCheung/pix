use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

/// Waits for one terminal event, returning `None` when the poll interval
/// expires. Keeping polling here gives the TUI one event source and makes
/// resize events available to the app without another terminal loop.
pub(crate) fn poll(timeout: Duration) -> io::Result<Option<Event>> {
    if event::poll(timeout)? {
        event::read().map(Some)
    } else {
        Ok(None)
    }
}

/// Returns whether an event asks the foundation app to leave the TUI.
pub(crate) fn is_quit(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };

    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return false;
    }

    matches!(key.code, KeyCode::Esc | KeyCode::Char('q' | 'Q'))
        || (matches!(key.code, KeyCode::Char('c' | 'C'))
            && key.modifiers.contains(KeyModifiers::CONTROL))
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::is_quit;

    #[test]
    fn quit_keys_include_escape_q_and_ctrl_c() {
        for key in [
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL),
        ] {
            assert!(is_quit(&Event::Key(key)));
        }
    }

    #[test]
    fn key_release_does_not_quit() {
        let mut key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;
        assert!(!is_quit(&Event::Key(key)));
    }

    #[test]
    fn resize_is_not_a_quit_event() {
        let event = Event::Resize(80, 24);
        assert!(!is_quit(&event));
    }
}
