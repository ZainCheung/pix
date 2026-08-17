//! Small terminal primitives used by `pix setup`.
//!
//! Setup is deliberately not a full-screen TUI. These helpers keep the
//! product-facing flow quiet, readable, and safe to use from a narrow
//! terminal while retaining a plain-text path for scripts and CI.

use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const WHITE: &str = "\x1b[97m";

/// The one terminal surface used by the setup wizard.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetupUi {
    interactive: bool,
    color: bool,
    verbose: bool,
}

/// Owns raw mode for exactly one prompt. It preserves an already-active raw
/// mode and always attempts to restore the previous state when the prompt
/// returns, errors, or is cancelled with Ctrl+C.
struct RawModeGuard {
    was_enabled: bool,
}

impl RawModeGuard {
    fn enter() -> Result<Self> {
        let was_enabled = is_raw_mode_enabled().context("checking terminal raw mode")?;
        if !was_enabled {
            enable_raw_mode().context("enabling terminal raw mode")?;
        }
        Ok(Self { was_enabled })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if !self.was_enabled {
            let _ = disable_raw_mode();
        }
    }
}

impl SetupUi {
    #[must_use]
    pub(crate) fn new(interactive: bool, verbose: bool) -> Self {
        let color =
            interactive && io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self {
            interactive,
            color,
            verbose,
        }
    }

    #[must_use]
    pub(crate) const fn interactive(self) -> bool {
        self.interactive
    }

    #[must_use]
    pub(crate) const fn verbose(self) -> bool {
        self.verbose
    }

    pub(crate) fn brand_header(self, subtitle: Option<&str>) {
        println!();
        println!("  {}", self.paint("pix", CYAN, true));
        if let Some(subtitle) = subtitle {
            println!("  {}", self.paint(subtitle, DIM, false));
        }
        println!();
    }

    pub(crate) fn crumb_header(self, section: &str) {
        println!();
        println!(
            "  {}  {}  {}",
            self.paint("pix", CYAN, true),
            self.paint("›", DIM, false),
            self.paint(section, DIM, false)
        );
        println!();
    }

    pub(crate) fn section(self, title: &str) {
        println!();
        println!("  {}", self.paint(title, WHITE, true));
        println!();
    }

    pub(crate) fn body(self, text: &str) {
        for line in text.lines() {
            println!("  {}", self.paint(line, WHITE, false));
        }
    }

    pub(crate) fn hint(self, text: &str) {
        for line in text.lines() {
            println!("  {}", self.paint(line, DIM, false));
        }
    }

    pub(crate) fn success(self, title: &str, detail: Option<&str>) {
        println!(
            "  {} {}",
            self.paint("✓", GREEN, false),
            self.paint(title, WHITE, false)
        );
        if let Some(detail) = detail {
            self.hint(&format!("  {detail}"));
        }
    }

    pub(crate) fn warning(self, title: &str, detail: Option<&str>) {
        println!(
            "  {} {}",
            self.paint("⚠", YELLOW, false),
            self.paint(title, WHITE, false)
        );
        if let Some(detail) = detail {
            self.hint(&format!("  {detail}"));
        }
    }

    pub(crate) fn error(self, title: &str, detail: Option<&str>) {
        println!(
            "  {} {}",
            self.paint("✕", RED, false),
            self.paint(title, WHITE, false)
        );
        if let Some(detail) = detail {
            self.hint(&format!("  {detail}"));
        }
    }

    pub(crate) fn muted(self, text: &str) {
        println!("  {}", self.paint(text, DIM, false));
    }

    /// Draws one task as a spinner. The completion call replaces this line in
    /// an interactive terminal and becomes a normal line in plain output.
    pub(crate) fn task(self, text: &str) {
        if self.interactive {
            print!("  {} {}", self.paint("⠋", CYAN, false), text);
            let _ = io::stdout().flush();
        } else {
            print!("{text}... ");
            let _ = io::stdout().flush();
        }
    }

    pub(crate) fn task_done(self, text: &str) {
        if self.interactive {
            print!("\r\x1b[2K");
            self.success(text, None);
        } else {
            println!("ok");
        }
    }

    pub(crate) fn task_failed(self, text: &str) {
        if self.interactive {
            print!("\r\x1b[2K");
            self.error(text, None);
        } else {
            println!("failed");
        }
    }

    /// A compact select. Empty input accepts the highlighted default. A
    /// numbered choice is supported as a portable fallback for terminals
    /// that do not expose raw key events; the visual contract remains a
    /// select rather than a Y/N prompt.
    pub(crate) fn select(self, prompt: &str, options: &[String], default: usize) -> Result<usize> {
        if options.is_empty() {
            return Ok(0);
        }
        self.ensure_tty()?;
        if self.interactive {
            return self.select_events(prompt, options, default);
        }
        self.select_line(prompt, options, default)
    }

    fn select_events(self, prompt: &str, options: &[String], default: usize) -> Result<usize> {
        let _raw_mode = RawModeGuard::enter()?;
        let mut selected = default.min(options.len() - 1);
        self.draw_select(prompt, options, selected, false)?;
        loop {
            let event = event::read().context("reading setup key event")?;
            let Event::Key(key) = event else {
                if matches!(event, Event::Resize(_, _)) {
                    self.draw_select(prompt, options, selected, true)?;
                }
                continue;
            };
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'C'))
            {
                Self::finish_prompt();
                bail!("setup cancelled by user");
            }
            match key.code {
                KeyCode::Up | KeyCode::Left => selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Right => {
                    selected = (selected + 1).min(options.len() - 1);
                }
                KeyCode::Home => selected = 0,
                KeyCode::End => selected = options.len() - 1,
                KeyCode::Enter => {
                    Self::finish_prompt();
                    return Ok(selected);
                }
                KeyCode::Esc => {
                    Self::finish_prompt();
                    bail!("setup cancelled by user");
                }
                KeyCode::Char(value) if value.is_ascii_digit() => {
                    if let Some(index) = value
                        .to_digit(10)
                        .and_then(|value| usize::try_from(value).ok())
                        .and_then(|value| value.checked_sub(1))
                        .filter(|index| *index < options.len())
                    {
                        selected = index;
                    }
                }
                _ => continue,
            }
            self.draw_select(prompt, options, selected, true)?;
        }
    }

    fn select_line(self, prompt: &str, options: &[String], default: usize) -> Result<usize> {
        let mut selected = default.min(options.len() - 1);
        if !prompt.trim().is_empty() {
            println!("  {}", self.paint(prompt, WHITE, true));
            println!();
        }
        for (index, option) in options.iter().enumerate() {
            let marker = if index == selected { "❯" } else { " " };
            let color = if index == selected { CYAN } else { WHITE };
            println!(
                "  {} {}",
                self.paint(marker, color, index == selected),
                self.paint(option, color, index == selected)
            );
        }
        println!();
        self.hint("↑↓ move   enter select");
        print!("  › ");
        io::stdout().flush().context("flushing setup selection")?;

        let mut line = String::new();
        let read = io::stdin()
            .read_line(&mut line)
            .context("reading setup selection")?;
        if read == 0 || line.trim().is_empty() {
            return Ok(selected);
        }
        let answer = line.trim();
        for byte in answer.as_bytes() {
            match *byte {
                b'A' => selected = selected.saturating_sub(1),
                b'B' => selected = (selected + 1).min(options.len() - 1),
                _ => {}
            }
        }
        if let Ok(index) = answer.parse::<usize>() {
            if let Some(index) = index.checked_sub(1).filter(|index| *index < options.len()) {
                selected = index;
            }
        } else if let Some(index) = options
            .iter()
            .position(|option| option.eq_ignore_ascii_case(answer))
        {
            selected = index;
        }
        Ok(selected)
    }

    fn draw_select(
        self,
        prompt: &str,
        options: &[String],
        selected: usize,
        redraw: bool,
    ) -> Result<()> {
        // Raw mode disables output post-processing on Unix. Keep every redraw
        // line explicitly CRLF-terminated so the cursor returns to column 0.
        if redraw {
            print!("\x1b[{}A\r", options.len() + 2);
        } else if !prompt.trim().is_empty() {
            print!("  {}\r\n\r\n", self.paint(prompt, WHITE, true));
        }
        for (index, option) in options.iter().enumerate() {
            if redraw {
                print!("\x1b[2K\r");
            }
            let marker = if index == selected { "❯" } else { " " };
            let color = if index == selected { CYAN } else { WHITE };
            print!(
                "  {} {}\r\n",
                self.paint(marker, color, index == selected),
                self.paint(option, color, index == selected)
            );
        }
        if redraw {
            print!("\x1b[2K\r");
        }
        print!("\r\n");
        self.raw_hint("↑↓ move   enter select");
        if redraw {
            print!("\x1b[2K\r");
        }
        print!("  › ");
        io::stdout().flush().context("flushing setup selection")?;
        Ok(())
    }

    /// A small multi-select primitive. It accepts comma-separated numbers in
    /// addition to the documented space-toggle mental model, which keeps the
    /// wizard usable when stdin is not a raw terminal.
    pub(crate) fn multiselect(
        self,
        prompt: &str,
        options: &[String],
        defaults: &[bool],
    ) -> Result<Vec<bool>> {
        if options.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_tty()?;
        if self.interactive {
            return self.multiselect_events(prompt, options, defaults);
        }
        self.multiselect_line(prompt, options, defaults)
    }

    fn multiselect_events(
        self,
        prompt: &str,
        options: &[String],
        defaults: &[bool],
    ) -> Result<Vec<bool>> {
        let _raw_mode = RawModeGuard::enter()?;
        let mut selected = options
            .iter()
            .enumerate()
            .map(|(index, _)| defaults.get(index).copied().unwrap_or(false))
            .collect::<Vec<_>>();
        let mut cursor = selected.iter().position(|checked| *checked).unwrap_or(0);
        self.draw_multiselect(prompt, options, &selected, cursor, false)?;
        loop {
            let event = event::read().context("reading setup multi-select key event")?;
            let Event::Key(key) = event else {
                if matches!(event, Event::Resize(_, _)) {
                    self.draw_multiselect(prompt, options, &selected, cursor, true)?;
                }
                continue;
            };
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'C'))
            {
                Self::finish_prompt();
                bail!("setup cancelled by user");
            }
            match key.code {
                KeyCode::Up | KeyCode::Left => cursor = cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Right => {
                    cursor = (cursor + 1).min(options.len().saturating_sub(1));
                }
                KeyCode::Home => cursor = 0,
                KeyCode::End => cursor = options.len().saturating_sub(1),
                KeyCode::Char(' ') => selected[cursor] = !selected[cursor],
                KeyCode::Enter => {
                    Self::finish_prompt();
                    return Ok(selected);
                }
                KeyCode::Esc => {
                    Self::finish_prompt();
                    bail!("setup cancelled by user");
                }
                _ => continue,
            }
            self.draw_multiselect(prompt, options, &selected, cursor, true)?;
        }
    }

    fn multiselect_line(
        self,
        prompt: &str,
        options: &[String],
        defaults: &[bool],
    ) -> Result<Vec<bool>> {
        let mut selected = options
            .iter()
            .enumerate()
            .map(|(index, _)| defaults.get(index).copied().unwrap_or(false))
            .collect::<Vec<_>>();
        println!("  {}", self.paint(prompt, WHITE, true));
        println!();
        for (index, option) in options.iter().enumerate() {
            let marker = if selected[index] { "x" } else { " " };
            println!(
                "  [{}] {}",
                self.paint(marker, CYAN, selected[index]),
                self.paint(option, WHITE, false)
            );
        }
        println!();
        self.hint("space toggle   enter continue");
        print!("  › ");
        io::stdout()
            .flush()
            .context("flushing setup multi-select")?;
        let mut line = String::new();
        let read = io::stdin()
            .read_line(&mut line)
            .context("reading setup multi-select")?;
        if read == 0 || line.trim().is_empty() {
            return Ok(selected);
        }
        selected.fill(false);
        for token in line.split([',', ' ', '\t']) {
            if let Ok(index) = token.trim().parse::<usize>()
                && let Some(value) = selected.get_mut(index.saturating_sub(1))
            {
                *value = true;
            }
        }
        Ok(selected)
    }

    fn draw_multiselect(
        self,
        prompt: &str,
        options: &[String],
        selected: &[bool],
        cursor: usize,
        redraw: bool,
    ) -> Result<()> {
        // See draw_select: raw mode does not translate LF to CRLF, so a bare
        // newline would make each subsequent redraw drift to the right.
        if redraw {
            print!("\x1b[{}A\r", options.len() + 2);
        } else {
            print!("  {}\r\n\r\n", self.paint(prompt, WHITE, true));
        }
        for (index, option) in options.iter().enumerate() {
            if redraw {
                print!("\x1b[2K\r");
            }
            let current = index == cursor;
            let marker = if current { "❯" } else { " " };
            let color = if current { CYAN } else { WHITE };
            let checked = if selected[index] { "x" } else { " " };
            print!(
                "  {} [{}] {}\r\n",
                self.paint(marker, color, current),
                self.paint(checked, CYAN, selected[index]),
                self.paint(option, color, current)
            );
        }
        if redraw {
            print!("\x1b[2K\r");
        }
        print!("\r\n");
        self.raw_hint("↑↓ move   space toggle   enter continue");
        if redraw {
            print!("\x1b[2K\r");
        }
        print!("  › ");
        io::stdout()
            .flush()
            .context("flushing setup multi-select")?;
        Ok(())
    }

    pub(crate) fn input(self, label: &str, default: Option<&str>) -> Result<String> {
        self.ensure_tty()?;
        println!("  {}", self.paint(label, WHITE, true));
        if let Some(default) = default.filter(|value| !value.is_empty()) {
            print!("  › {} ", self.paint(default, CYAN, false));
        } else {
            print!("  › ");
        }
        io::stdout().flush().context("flushing setup input")?;
        let mut line = String::new();
        let read = io::stdin()
            .read_line(&mut line)
            .context("reading setup input")?;
        if read == 0 {
            return Ok(default.unwrap_or_default().to_owned());
        }
        let value = line.trim();
        if value.is_empty() {
            Ok(default.unwrap_or_default().to_owned())
        } else {
            Ok(value.to_owned())
        }
    }

    #[must_use]
    pub(crate) fn paint(self, text: &str, color: &str, bold: bool) -> String {
        if !self.color {
            return text.to_owned();
        }
        let weight = if bold { BOLD } else { "" };
        format!("{weight}{color}{text}{RESET}")
    }

    #[must_use]
    pub(crate) fn cyan(self, text: &str, bold: bool) -> String {
        self.paint(text, CYAN, bold)
    }

    #[must_use]
    pub(crate) fn green(self, text: &str, bold: bool) -> String {
        self.paint(text, GREEN, bold)
    }

    fn ensure_tty(self) -> Result<()> {
        if self.interactive && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
            bail!("interactive setup requires stdin and stdout to be TTYs");
        }
        Ok(())
    }

    fn raw_hint(self, text: &str) {
        for line in text.lines() {
            print!("  {}\r\n", self.paint(line, DIM, false));
        }
    }

    fn finish_prompt() {
        print!("\r\x1b[2K\r\n");
        let _ = io::stdout().flush();
    }
}

/// A small helper used by tests and by the QR renderer to keep the terminal
/// output bounded. It deliberately does not inspect the encoded payload.
#[must_use]
pub(crate) fn clamp_text(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.push('…');
    }
    output
}
