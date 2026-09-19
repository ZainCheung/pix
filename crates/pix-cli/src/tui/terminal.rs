use std::io::{self, Stdout, Write};

use anyhow::{Context, Result};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    is_raw_mode_enabled,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
#[cfg(test)]
use ratatui::layout::Rect;
#[cfg(test)]
use ratatui::{TerminalOptions, Viewport};

/// Owns every terminal mutation made by the Ratatui frontend.
///
/// The guard is intentionally generic over its writer so lifecycle cleanup
/// can be exercised with an in-memory writer in unit tests. Production entry
/// uses `Stdout` and a single Crossterm backend.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct TerminalGuard<W: Write = Stdout> {
    terminal: Terminal<CrosstermBackend<W>>,
    restored: bool,
    raw_mode_owned: bool,
    alternate_screen_entered: bool,
    cursor_hidden: bool,
}

impl TerminalGuard<Stdout> {
    pub(crate) fn enter() -> Result<Self> {
        let raw_mode_owned = !is_raw_mode_enabled().context("checking terminal raw mode")?;
        if raw_mode_owned {
            enable_raw_mode().context("enabling terminal raw mode")?;
        }

        let terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = restore_owned_raw_mode(raw_mode_owned);
                return Err(error).context("initializing Ratatui terminal");
            }
        };
        let mut guard = Self {
            terminal,
            restored: false,
            raw_mode_owned,
            alternate_screen_entered: false,
            cursor_hidden: false,
        };

        if let Err(error) = execute!(guard.terminal.backend_mut(), EnterAlternateScreen) {
            let _ = guard.restore();
            return Err(error).context("entering terminal alternate screen");
        }
        guard.alternate_screen_entered = true;

        if let Err(error) = execute!(guard.terminal.backend_mut(), crossterm::cursor::Hide) {
            let _ = guard.restore();
            return Err(error).context("hiding terminal cursor");
        }
        guard.cursor_hidden = true;

        // `Terminal::clear` preserves the cursor by querying it with an
        // escape sequence. Some minimal PTYs do not answer that query, while
        // a fresh alternate screen needs no cursor preservation. Clear the
        // backend directly so entry remains safe in those terminals too.
        if let Err(error) = guard.terminal.backend_mut().clear() {
            let _ = guard.restore();
            return Err(error).context("clearing Ratatui terminal");
        }

        Ok(guard)
    }
}

impl<W: Write> TerminalGuard<W> {
    pub(crate) fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<W>> {
        &mut self.terminal
    }

    /// Restores the terminal once, attempting every cleanup operation even
    /// when an earlier operation fails. The first cleanup error is returned;
    /// callers can preserve an earlier application error over it.
    pub(crate) fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }

        let mut first_error = None;
        if self.alternate_screen_entered {
            match execute!(self.terminal.backend_mut(), LeaveAlternateScreen) {
                Ok(()) => self.alternate_screen_entered = false,
                Err(error) => record_first_error(&mut first_error, Err(error)),
            }
        }

        if self.cursor_hidden {
            match self.terminal.show_cursor() {
                Ok(()) => self.cursor_hidden = false,
                Err(error) => record_first_error(&mut first_error, Err(error)),
            }
        }

        record_first_error(
            &mut first_error,
            Backend::flush(self.terminal.backend_mut()),
        );

        if self.raw_mode_owned {
            match disable_raw_mode() {
                Ok(()) => self.raw_mode_owned = false,
                Err(error) => record_first_error(&mut first_error, Err(error)),
            }
        }

        self.restored = !self.raw_mode_owned
            && !self.alternate_screen_entered
            && !self.cursor_hidden
            && first_error.is_none();
        first_error.map_or(Ok(()), Err)
    }

    #[cfg(test)]
    fn for_test(writer: W) -> io::Result<Self> {
        Ok(Self {
            // A fixed viewport avoids Crossterm's terminal-size query, so
            // lifecycle tests remain deterministic on headless CI runners.
            terminal: Terminal::with_options(
                CrosstermBackend::new(writer),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
                },
            )?,
            restored: false,
            raw_mode_owned: false,
            alternate_screen_entered: false,
            cursor_hidden: false,
        })
    }
}

fn record_first_error(first_error: &mut Option<io::Error>, result: io::Result<()>) {
    if first_error.is_none() {
        *first_error = result.err();
    }
}

impl<W: Write> Drop for TerminalGuard<W> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn restore_owned_raw_mode(raw_mode_owned: bool) -> io::Result<()> {
    if raw_mode_owned {
        disable_raw_mode()
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::TerminalGuard;

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("test terminal write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("test terminal flush failure"))
        }
    }

    #[test]
    fn restore_is_idempotent() {
        let mut guard = TerminalGuard::for_test(Vec::<u8>::new()).expect("test terminal");
        guard.restore().expect("first restore");
        guard.restore().expect("second restore");
    }

    #[test]
    fn partial_entry_cleanup_is_best_effort() {
        let mut guard = TerminalGuard::for_test(Vec::<u8>::new()).expect("test terminal");
        guard.alternate_screen_entered = true;
        guard.cursor_hidden = true;
        guard.restore().expect("restore partial terminal");
        assert!(guard.restored);
        assert!(!guard.alternate_screen_entered);
        assert!(!guard.cursor_hidden);
        assert!(!guard.raw_mode_owned);
    }

    #[test]
    fn drop_does_not_panic() {
        let guard = TerminalGuard::for_test(Vec::<u8>::new()).expect("test terminal");
        drop(guard);
    }

    #[test]
    fn cleanup_marks_guard_restored_when_writes_fail() {
        let mut guard = TerminalGuard::for_test(FailingWriter).expect("test terminal");
        guard.alternate_screen_entered = true;
        guard.cursor_hidden = true;
        assert!(guard.restore().is_err());
        assert!(!guard.restored);
        assert!(guard.alternate_screen_entered);
        assert!(guard.cursor_hidden);
    }

    #[test]
    fn cleanup_retries_pending_terminal_state() {
        let mut guard =
            TerminalGuard::for_test(FailOnceWriter { failed: false }).expect("test terminal");
        guard.alternate_screen_entered = true;

        assert!(guard.restore().is_err());
        assert!(!guard.restored);
        assert!(guard.alternate_screen_entered);

        guard.restore().expect("retry restore");
        assert!(guard.restored);
        assert!(!guard.alternate_screen_entered);
    }

    #[test]
    fn preexisting_raw_mode_is_not_owned_by_the_guard() {
        let mut guard = TerminalGuard::for_test(Vec::<u8>::new()).expect("test terminal");
        guard.raw_mode_owned = false;
        guard.restore().expect("restore preexisting raw mode");
        assert!(!guard.raw_mode_owned);
    }

    struct FailOnceWriter {
        failed: bool,
    }

    impl Write for FailOnceWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if !self.failed {
                self.failed = true;
                return Err(io::Error::other("test transient terminal write failure"));
            }
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
