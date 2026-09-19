mod event;
mod terminal;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::Event;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::home::{ConfigState, HostOverview};
use crate::output::OutputFormat;

pub(crate) use terminal::TerminalGuard;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractionMode {
    Tui,
    HumanCli,
    Machine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TtyState {
    Attached,
    Detached,
}

impl TtyState {
    pub(crate) const fn from_is_tty(is_tty: bool) -> Self {
        if is_tty {
            Self::Attached
        } else {
            Self::Detached
        }
    }
}

/// Resolves the frontend before any operation can print or mutate terminal
/// state. Explicit commands always remain human CLI even when attached to a
/// TTY; only bare, usable interactive terminals may enter the TUI.
pub(crate) fn interaction_mode(
    has_command: bool,
    no_input: bool,
    output: OutputFormat,
    stdin: TtyState,
    stdout: TtyState,
    term: Option<&str>,
) -> InteractionMode {
    if output == OutputFormat::Json {
        return InteractionMode::Machine;
    }
    if has_command
        || no_input
        || stdin == TtyState::Detached
        || stdout == TtyState::Detached
        || !usable_term(term)
    {
        return InteractionMode::HumanCli;
    }
    InteractionMode::Tui
}

pub(crate) fn should_launch_tui(
    has_command: bool,
    no_input: bool,
    output: OutputFormat,
    stdin: TtyState,
    stdout: TtyState,
    term: Option<&str>,
) -> bool {
    interaction_mode(has_command, no_input, output, stdin, stdout, term) == InteractionMode::Tui
}

fn usable_term(term: Option<&str>) -> bool {
    term.is_some_and(|value| {
        let value = value.trim();
        !value.is_empty() && !value.eq_ignore_ascii_case("dumb")
    })
}

/// Runs the foundation screen. Feature pages intentionally remain on the
/// existing prompt-based paths until their respective migrations land.
pub(crate) fn run(overview: &HostOverview) -> Result<()> {
    let mut guard = TerminalGuard::enter()?;
    let result = run_loop(&mut guard, overview);
    let cleanup = guard.restore();

    match result {
        Err(error) => Err(error),
        Ok(()) => cleanup.map_err(Into::into),
    }
}

fn run_loop(guard: &mut TerminalGuard, overview: &HostOverview) -> Result<()> {
    loop {
        guard.terminal_mut().draw(|frame| render(frame, overview))?;
        if let Some(event) = event::poll(Duration::from_millis(250))? {
            match event {
                Event::Resize(_, _) => {
                    // Ratatui reads the new frame area on the next draw. No
                    // cursor arithmetic or nested redraw loop is required.
                }
                event if event::is_quit(&event) => break,
                _ => {}
            }
        }
    }
    Ok(())
}

fn render(frame: &mut Frame<'_>, overview: &HostOverview) {
    let [body, footer_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .areas(frame.area());

    let title = Block::default()
        .title(" Pix ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = title.inner(body);
    frame.render_widget(title, body);

    let host = overview.host.as_deref().unwrap_or("not configured");
    let config = match overview.config_state {
        ConfigState::Ready => "ready",
        ConfigState::Missing => "not configured",
        ConfigState::Invalid => "needs attention",
    };
    let lines = vec![
        Line::from(Span::styled(
            "TUI foundation ready",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("Host       {host}")),
        Line::from(format!("Config     {config}")),
        Line::from(format!("Workspaces {} authorized", overview.workspaces)),
        Line::from(format!("Devices    {} paired", overview.devices)),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q ", Style::default().fg(Color::Cyan)),
        Span::raw("Quit  "),
        Span::styled("Ctrl-C", Style::default().fg(Color::Cyan)),
        Span::raw(" Quit"),
    ]));
    frame.render_widget(footer, footer_area);
}

#[cfg(test)]
mod tests {
    use super::{InteractionMode, TtyState, interaction_mode, should_launch_tui};
    use crate::output::OutputFormat;

    const TTY: TtyState = TtyState::Attached;

    #[test]
    fn bare_interactive_human_terminal_enters_tui() {
        assert!(should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TTY,
            TTY,
            Some("xterm-256color")
        ));
        assert_eq!(
            interaction_mode(
                false,
                false,
                OutputFormat::Human,
                TTY,
                TTY,
                Some("xterm-256color")
            ),
            InteractionMode::Tui
        );
    }

    #[test]
    fn explicit_status_command_stays_human_cli() {
        assert_eq!(
            interaction_mode(true, false, OutputFormat::Human, TTY, TTY, Some("xterm")),
            InteractionMode::HumanCli
        );
    }

    #[test]
    fn explicit_workspace_command_stays_human_cli() {
        assert!(!should_launch_tui(
            true,
            false,
            OutputFormat::Human,
            TTY,
            TTY,
            Some("xterm")
        ));
    }

    #[test]
    fn machine_output_never_enters_tui() {
        assert_eq!(
            interaction_mode(false, false, OutputFormat::Json, TTY, TTY, Some("xterm")),
            InteractionMode::Machine
        );
    }

    #[test]
    fn no_input_and_non_tty_never_enter_tui() {
        assert!(!should_launch_tui(
            false,
            true,
            OutputFormat::Human,
            TTY,
            TTY,
            Some("xterm")
        ));
        assert!(!should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TtyState::Detached,
            TTY,
            Some("xterm")
        ));
        assert!(!should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TTY,
            TtyState::Detached,
            Some("xterm")
        ));
    }

    #[test]
    fn unusable_term_never_enters_tui() {
        assert!(!should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TTY,
            TTY,
            None
        ));
        assert!(!should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TTY,
            TTY,
            Some("dumb")
        ));
    }
}
