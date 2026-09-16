use std::io::{self, Stdout, Write};

use anyhow::{Context, Result};
use crossterm::cursor::Show;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};

/// Owns every terminal mutation made by the Ratatui frontend.
///
/// The guard is intentionally generic over its writer so lifecycle cleanup
/// can be exercised with an in-memory writer in unit tests. Production entry
/// uses `Stdout` and a single Crossterm backend.
pub(crate) struct TerminalGuard<W: Write = Stdout> {
    terminal: Terminal<CrosstermBackend<W>>,
    restored: bool,
    raw_mode_enabled: bool,
    alternate_screen_entered: bool,
}

impl TerminalGuard<Stdout> {
    pub(crate) fn enter() -> Result<Self> {
        enable_raw_mode().context("enabling terminal raw mode")?;

        let terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = restore_stdout();
                return Err(error).context("initializing Ratatui terminal");
            }
        };
        let mut guard = Self {
            terminal,
            restored: false,
            raw_mode_enabled: true,
            alternate_screen_entered: false,
        };

        // Mark the screen as entered before issuing the command sequence so a
        // partial write (for example, EnterAlternateScreen succeeds but Hide
        // fails) still receives a best-effort LeaveAlternateScreen.
        guard.alternate_screen_entered = true;
        if let Err(error) = execute!(
            guard.terminal.backend_mut(),
            EnterAlternateScreen,
            crossterm::cursor::Hide
        ) {
            let _ = guard.restore();
            return Err(error).context("entering terminal alternate screen");
        }

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
            record_first_error(
                &mut first_error,
                execute!(self.terminal.backend_mut(), LeaveAlternateScreen),
            );
            self.alternate_screen_entered = false;
        }

        record_first_error(&mut first_error, self.terminal.show_cursor());
        record_first_error(
            &mut first_error,
            Backend::flush(self.terminal.backend_mut()),
        );

        if self.raw_mode_enabled {
            record_first_error(&mut first_error, disable_raw_mode());
            self.raw_mode_enabled = false;
        }

        self.restored = true;
        first_error.map_or(Ok(()), Err)
    }

    #[cfg(test)]
    fn for_test(writer: W) -> io::Result<Self> {
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(writer))?,
            restored: false,
            raw_mode_enabled: false,
            alternate_screen_entered: false,
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

fn restore_stdout() -> io::Result<()> {
    let mut stdout = io::stdout();
    let mut first_error = None;

    record_first_error(
        &mut first_error,
        execute!(stdout, LeaveAlternateScreen, Show),
    );
    record_first_error(&mut first_error, stdout.flush());
    record_first_error(&mut first_error, disable_raw_mode());

    first_error.map_or(Ok(()), Err)
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
        guard.restore().expect("restore partial terminal");
        assert!(guard.restored);
        assert!(!guard.alternate_screen_entered);
        assert!(!guard.raw_mode_enabled);
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
        assert!(guard.restore().is_err());
        assert!(guard.restored);
        assert!(!guard.alternate_screen_entered);
    }
}
