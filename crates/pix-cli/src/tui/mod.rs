mod event;
mod terminal;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::home::{AccessMode, ConfigState, HostOverview, ServiceState};
use crate::output::OutputFormat;
use crate::setup_ui::LOGO;

pub(crate) use terminal::TerminalGuard;

const MIN_WIDTH: u16 = 48;
const HOME_BANNER_HEIGHT: u16 = 8;
const HOME_SUMMARY_HEIGHT: u16 = 6;
const HOME_MENU_HEIGHT: u16 = 4;
const FOOTER_HEIGHT: u16 = 1;
const MIN_HEIGHT: u16 = HOME_BANNER_HEIGHT + HOME_SUMMARY_HEIGHT + HOME_MENU_HEIGHT + FOOTER_HEIGHT;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Home,
    Devices,
    Workspaces,
    Settings,
    Status,
}

impl Route {
    const fn title(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Devices => "Devices",
            Self::Workspaces => "Workspaces",
            Self::Settings => "Settings",
            Self::Status => "Status",
        }
    }

    const fn from_home_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Devices),
            1 => Some(Self::Workspaces),
            2 => Some(Self::Settings),
            3 => Some(Self::Status),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastTone {
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Toast {
    pub(crate) message: String,
    pub(crate) tone: ToastTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TerminalSize {
    pub(crate) width: u16,
    pub(crate) height: u16,
}

/// Persistent state for the interactive Pix application.
///
/// Screens only render from this state. They do not own a nested event loop,
/// raw mode, or terminal cleanup, which keeps route changes inside one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct App {
    pub(crate) route: Route,
    pub(crate) history: Vec<Route>,
    selection_history: Vec<usize>,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) toast: Option<Toast>,
    pub(crate) should_quit: bool,
    pub(crate) selected: usize,
    pub(crate) terminal_size: TerminalSize,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub(crate) fn new() -> Self {
        Self {
            route: Route::Home,
            history: Vec::new(),
            selection_history: Vec::new(),
            overlay: None,
            toast: None,
            should_quit: false,
            selected: 0,
            terminal_size: TerminalSize::default(),
        }
    }

    pub(crate) const fn is_home(&self) -> bool {
        matches!(self.route, Route::Home)
    }

    pub(crate) fn set_terminal_size(&mut self, size: TerminalSize) {
        self.terminal_size = size;
    }

    pub(crate) fn handle_event(&mut self, event: &Event) {
        if let Event::Resize(width, height) = event {
            self.set_terminal_size(TerminalSize {
                width: *width,
                height: *height,
            });
            return;
        }

        let Event::Key(key) = event else {
            return;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }

        // Ctrl-C is a global action. Handle it before overlays so the help
        // surface cannot swallow the quit contract it advertises.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            self.should_quit = true;
            return;
        }

        if self.overlay.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?' | 'q' | 'Q') => {
                    self.overlay = None;
                }
                _ => {}
            }
            return;
        }

        if event::is_quit(event) {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                self.should_quit = true;
            } else {
                self.go_back_or_quit();
            }
            return;
        }

        match key.code {
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.max_selection(),
            KeyCode::Enter => self.activate_selection(),
            KeyCode::Char(value) if value.is_ascii_digit() && self.is_home() => {
                if let Some(index) = value
                    .to_digit(10)
                    .and_then(|value| usize::try_from(value).ok())
                    .and_then(|value| value.checked_sub(1))
                    .and_then(Route::from_home_index)
                {
                    self.open(index);
                }
            }
            _ => {}
        }
    }

    fn max_selection(&self) -> usize {
        if self.is_home() { 3 } else { 0 }
    }

    fn move_selection(&mut self, direction: i8) {
        let max = self.max_selection();
        self.selected = if direction.is_negative() {
            self.selected.saturating_sub(1)
        } else {
            (self.selected + 1).min(max)
        };
    }

    fn activate_selection(&mut self) {
        if self.is_home() {
            if let Some(route) = Route::from_home_index(self.selected) {
                self.open(route);
            }
        } else {
            self.toast = Some(Toast {
                message: format!(
                    "{} actions are coming in a follow-up issue",
                    self.route.title()
                ),
                tone: ToastTone::Info,
            });
        }
    }

    fn open(&mut self, route: Route) {
        if route == self.route {
            return;
        }
        self.history.push(self.route);
        self.selection_history.push(self.selected);
        self.route = route;
        self.selected = 0;
        self.toast = None;
    }

    fn go_back_or_quit(&mut self) {
        if let Some(route) = self.history.pop() {
            self.route = route;
            self.selected = self.selection_history.pop().unwrap_or(0);
            self.toast = None;
        } else {
            self.should_quit = true;
        }
    }
}

/// Runs the persistent application shell. Feature pages intentionally remain
/// placeholders until their respective migrations land.
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
    let mut app = App::new();
    loop {
        guard.terminal_mut().draw(|frame| {
            app.set_terminal_size(TerminalSize {
                width: frame.area().width,
                height: frame.area().height,
            });
            render(frame, &app, overview);
        })?;

        if app.should_quit {
            break;
        }
        if let Some(event) = event::poll(Duration::from_millis(250))? {
            app.handle_event(&event);
        }
    }
    Ok(())
}

fn render(frame: &mut Frame<'_>, app: &App, overview: &HostOverview) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_small_terminal(frame, area);
        if app.overlay.is_some() {
            render_help(frame, area);
        }
        return;
    }

    let toast_height = usize::from(app.toast.is_some());
    let [body, toast, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(u16::try_from(toast_height).unwrap_or(0)),
            Constraint::Length(1),
        ])
        .areas(area);

    match app.route {
        Route::Home => render_home(frame, body, app, overview),
        route => render_child(frame, body, route, overview),
    }
    if let Some(toast_value) = &app.toast {
        render_toast(frame, toast, toast_value);
    }
    render_footer(frame, footer, app.route);

    if app.overlay.is_some() {
        render_help(frame, area);
    }
}

fn render_small_terminal(frame: &mut Frame<'_>, area: Rect) {
    let message = vec![
        Line::from(Span::styled(
            "Pix needs a larger terminal",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Resize to at least {MIN_WIDTH}×{MIN_HEIGHT}; press q to go back or quit."
        )),
    ];
    frame.render_widget(
        Paragraph::new(message)
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

#[allow(clippy::too_many_lines)]
fn render_home(frame: &mut Frame<'_>, area: Rect, app: &App, overview: &HostOverview) {
    let [banner, summary, menu] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HOME_BANNER_HEIGHT),
            Constraint::Length(HOME_SUMMARY_HEIGHT),
            Constraint::Min(1),
        ])
        .areas(area);

    let logo_lines = LOGO
        .lines()
        .enumerate()
        .map(|(index, value)| {
            let mut spans = vec![Span::styled(
                format!("  {value:<20}"),
                Style::default().fg(Color::Cyan),
            )];
            if index == 4 {
                spans.push(Span::styled(
                    "pix.deepoke.com",
                    Style::default().fg(Color::Cyan),
                ));
            } else if index == 5 {
                spans.push(Span::styled(
                    "Remote access for the Pi agent",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        })
        .chain([
            Line::from(Span::styled(
                format!("  pix {}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(Color::DarkGray),
            )),
            version_mismatch_hint(overview),
        ])
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(logo_lines), banner);

    let summary_lines = vec![
        status_line(
            "Host",
            overview.host.as_deref().unwrap_or("not configured"),
            Style::default().fg(match overview.config_state {
                ConfigState::Ready => Color::White,
                ConfigState::Missing => Color::Yellow,
                ConfigState::Invalid => Color::Red,
            }),
        ),
        status_line(
            "Pi",
            pi_summary(overview),
            Style::default().fg(Color::DarkGray),
        ),
        status_line(
            "Service",
            service_summary(overview),
            service_style(overview),
        ),
        status_line("Relay", relay_summary(overview), relay_style(overview)),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!(
                    "{} paired  ·  {} authorized",
                    overview.devices, overview.workspaces
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(summary_lines), summary);

    let descriptions = [
        (
            "Devices",
            "Pair, approve, or revoke a phone",
            format!("{} paired", overview.devices),
        ),
        (
            "Workspaces",
            "Manage authorized folders",
            format!("{} authorized", overview.workspaces),
        ),
        ("Settings", "Configure remote access", String::new()),
        ("Status", "Inspect detailed host state", String::new()),
    ];
    let show_descriptions = area.width >= 68;
    let menu_lines = descriptions
        .iter()
        .enumerate()
        .map(|(index, (label, description, detail))| {
            let selected = index == app.selected;
            let marker_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let label_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let detail_style = Style::default().fg(Color::DarkGray);
            let detail_text = if show_descriptions && detail.is_empty() {
                (*description).to_string()
            } else if show_descriptions {
                format!("{description}  {detail}")
            } else {
                detail.clone()
            };
            Line::from(vec![
                Span::styled(if selected { "❯" } else { " " }, marker_style),
                Span::raw(" "),
                Span::styled(format!("{}. {:<12}", index + 1, label), label_style),
                Span::styled(detail_text, detail_style),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(menu_lines).wrap(Wrap { trim: true }), menu);
}

fn render_child(frame: &mut Frame<'_>, area: Rect, route: Route, overview: &HostOverview) {
    let detail = match route {
        Route::Devices => format!("{} paired", overview.devices),
        Route::Workspaces => format!("{} authorized", overview.workspaces),
        Route::Settings | Route::Status | Route::Home => String::new(),
    };
    let header = Line::from(vec![
        Span::styled(
            "pix",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" › ", Style::default().fg(Color::DarkGray)),
        Span::styled(route.title(), Style::default().fg(Color::White)),
        if detail.is_empty() {
            Span::raw("")
        } else {
            Span::styled(format!("  {detail}"), Style::default().fg(Color::DarkGray))
        },
    ]);

    let content = match route {
        Route::Devices if overview.devices == 0 => vec![
            header,
            Line::from(""),
            Line::from(Span::styled(
                "No paired devices yet.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from("Pairing actions will appear here when device management lands."),
        ],
        Route::Workspaces if overview.workspaces == 0 => vec![
            header,
            Line::from(""),
            Line::from(Span::styled(
                "No authorized workspaces yet.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from("Workspace actions will appear here in the next migration."),
        ],
        Route::Devices | Route::Workspaces => vec![
            header,
            Line::from(""),
            Line::from("Use Enter for the selected item. Add and removal actions are reserved"),
            Line::from("for the device/workspace feature migrations."),
        ],
        Route::Settings => vec![
            header,
            Line::from(""),
            status_line("Relay", relay_summary(overview), relay_style(overview)),
            status_line(
                "Pi",
                pi_summary(overview),
                Style::default().fg(Color::DarkGray),
            ),
            Line::from("Settings controls will be added without changing explicit CLI commands."),
        ],
        Route::Status => vec![
            header,
            Line::from(""),
            status_line(
                "Config",
                match overview.config_state {
                    ConfigState::Ready => "ready",
                    ConfigState::Missing => "not configured",
                    ConfigState::Invalid => "needs attention",
                },
                Style::default().fg(Color::DarkGray),
            ),
            status_line(
                "Host",
                overview.host.as_deref().unwrap_or("not configured"),
                Style::default(),
            ),
            status_line(
                "Service",
                service_summary(overview),
                service_style(overview),
            ),
            status_line(
                "Config path",
                &overview.config_path,
                Style::default().fg(Color::DarkGray),
            ),
        ],
        Route::Home => unreachable!("home is rendered separately"),
    };
    frame.render_widget(Paragraph::new(content).wrap(Wrap { trim: true }), area);
}

fn render_toast(frame: &mut Frame<'_>, area: Rect, toast: &Toast) {
    let style = match toast.tone {
        ToastTone::Info => Style::default().fg(Color::Cyan),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ", style),
            Span::styled(&toast.message, style),
        ])),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, route: Route) {
    let hints = footer_hints(area.width, route);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled(hints, Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn footer_hints(width: u16, route: Route) -> &'static str {
    if width < 72 {
        match route {
            Route::Home => "↑↓/jk Move   ↵ Open   ? Help   Q Quit",
            Route::Devices | Route::Workspaces | Route::Settings | Route::Status => {
                "Esc/q Back   ? Help"
            }
        }
    } else {
        match route {
            Route::Home => "↑↓/jk Navigate   ↵ Open   1–4 Jump   ? Help   Q Quit",
            Route::Devices | Route::Workspaces | Route::Settings | Route::Status => {
                "Esc/q Back   ? Help"
            }
        }
    }
}

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let width = 60.min(area.width.saturating_sub(2));
    let height = 12.min(area.height.saturating_sub(2));
    if width == 0 || height == 0 {
        return;
    }
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Pix navigation",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("↑↓ or j/k   move the current selection"),
            Line::from("Enter       open or select"),
            Line::from("Esc / q     go back; quit from Home"),
            Line::from("?           close this help"),
            Line::from("Ctrl-C      quit Pix"),
            Line::from(""),
            Line::from("Resize the terminal to redraw the current frame."),
        ])
        .block(Block::default().borders(Borders::ALL).title(" Help "))
        .wrap(Wrap { trim: true }),
        popup,
    );
}

fn version_mismatch_message(host_version: &str) -> String {
    format!(
        "  CLI {} · Host {host_version} — restart the host service to use the current CLI version",
        env!("CARGO_PKG_VERSION")
    )
}

fn version_mismatch_hint(overview: &HostOverview) -> Line<'static> {
    match overview.service.pix_version.as_deref() {
        Some(version) if version != env!("CARGO_PKG_VERSION") => Line::from(Span::styled(
            version_mismatch_message(version),
            Style::default().fg(Color::Yellow),
        )),
        _ => Line::from(""),
    }
}

fn status_line(label: &str, value: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<10}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value.into(), style),
    ])
}

fn pi_summary(overview: &HostOverview) -> String {
    match overview.pi.version.as_deref() {
        Some(version) => version.to_owned(),
        None => match overview.pi.compatibility {
            Some(pix_core::PiCompatibilityStatus::NotFound) => "not found".to_owned(),
            Some(pix_core::PiCompatibilityStatus::CannotLaunch) => "could not start".to_owned(),
            _ => "not detected".to_owned(),
        },
    }
}

fn service_summary(overview: &HostOverview) -> String {
    match overview.service.state {
        ServiceState::Running => "● running".to_owned(),
        ServiceState::Stopped => "○ installed, stopped".to_owned(),
        ServiceState::NotInstalled => "○ not installed".to_owned(),
    }
}

fn service_style(overview: &HostOverview) -> Style {
    Style::default().fg(match overview.service.state {
        ServiceState::Running => Color::Green,
        ServiceState::Stopped => Color::Yellow,
        ServiceState::NotInstalled => Color::DarkGray,
    })
}

fn relay_summary(overview: &HostOverview) -> String {
    match overview.access.mode {
        AccessMode::Relay => "● enabled".to_owned(),
        AccessMode::RelayDisabled => "○ disabled".to_owned(),
        AccessMode::Local => "○ local network only".to_owned(),
        AccessMode::Unknown => "unknown".to_owned(),
    }
}

fn relay_style(overview: &HostOverview) -> Style {
    Style::default().fg(match overview.access.mode {
        AccessMode::Relay => Color::Green,
        AccessMode::RelayDisabled => Color::Yellow,
        AccessMode::Local => Color::DarkGray,
        AccessMode::Unknown => Color::Red,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        App, InteractionMode, MIN_HEIGHT, Route, TerminalSize, TtyState, footer_hints,
        interaction_mode, should_launch_tui, version_mismatch_message,
    };
    use crate::output::OutputFormat;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

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

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn home_navigation_opens_and_back_returns_to_the_same_shell() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Down));
        app.handle_event(&key(KeyCode::Enter));
        assert_eq!(app.route, Route::Workspaces);
        assert_eq!(app.history, vec![Route::Home]);

        app.handle_event(&key(KeyCode::Esc));
        assert_eq!(app.route, Route::Home);
        assert_eq!(app.selected, 1);
        assert!(app.history.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn home_number_shortcuts_open_routes() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Char('4')));
        assert_eq!(app.route, Route::Status);
        app.handle_event(&key(KeyCode::Char('q')));
        assert_eq!(app.route, Route::Home);
        assert!(!app.should_quit);
        app.handle_event(&key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn help_is_a_shared_overlay_and_escape_closes_it() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Char('?')));
        assert!(app.overlay.is_some());
        app.handle_event(&key(KeyCode::Esc));
        assert!(app.overlay.is_none());
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_quits_even_when_help_overlay_is_open() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Char('?')));
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit);
    }

    #[test]
    fn minimum_terminal_height_leaves_room_for_the_home_menu() {
        assert_eq!(MIN_HEIGHT, 19);
    }

    #[test]
    fn placeholder_footers_only_advertise_implemented_actions() {
        for route in [Route::Devices, Route::Workspaces, Route::Settings] {
            assert_eq!(footer_hints(80, route), "Esc/q Back   ? Help");
        }
    }

    #[test]
    fn version_mismatch_message_is_neutral_about_update_direction() {
        for host_version in ["0.1.6", "0.1.8"] {
            let message = version_mismatch_message(host_version);
            assert!(message.contains(&format!("CLI {}", env!("CARGO_PKG_VERSION"))));
            assert!(message.contains(&format!("Host {host_version}")));
            assert!(message.contains("restart the host service"));
            assert!(!message.contains("after updating"));
        }
    }

    #[test]
    fn resize_updates_the_app_without_changing_the_route() {
        let mut app = App::new();
        app.handle_event(&Event::Resize(120, 40));
        assert_eq!(
            app.terminal_size,
            TerminalSize {
                width: 120,
                height: 40
            }
        );
        assert_eq!(app.route, Route::Home);
    }

    #[test]
    fn q_quits_only_after_the_route_stack_is_empty() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Enter));
        app.handle_event(&key(KeyCode::Char('q')));
        assert_eq!(app.route, Route::Home);
        assert!(!app.should_quit);
        app.handle_event(&key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }
}
