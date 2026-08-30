use std::io::IsTerminal;

use std::path::PathBuf;

use std::process::ExitCode;

use anyhow::{Context, Result};

use clap::{CommandFactory, Parser, Subcommand};

use pix_core::ConfigStore;

mod commands;
mod diagnostics;
mod home;
mod output;
mod serve;
mod service;
mod service_client;
mod setup_ui;
mod status;

use crate::commands::device::device;
use crate::commands::pi::pi_command;
use crate::commands::relay::relay_command;
use crate::commands::session::session;
use crate::commands::setup::{SetupOptions, default_setup_options, setup};
use crate::commands::update::update;
use crate::commands::workspace::workspace;
use crate::diagnostics::diagnostics_command;
use crate::home::{HomeAction, HostOverview};
use crate::output::{CliUsageError, CommandOutput, OutputFormat};
use crate::serve::serve;
use crate::service::ServiceCommand;
use crate::service::service_command;
use crate::setup_ui::SetupUi;
use crate::status::{show_logs, status_command};

#[derive(Debug, Parser)]
#[command(name = "pix", version, about = "Pix remote access for Pi")]
struct Cli {
    /// Override the platform Pix configuration path.
    #[arg(long, global = true, env = "PIX_CONFIG")]
    config: Option<PathBuf>,
    /// Select human-readable output or the versioned machine-readable JSON
    /// contract. JSON output never prompts for input.
    #[arg(long, global = true, env = "PIX_OUTPUT", value_enum, default_value_t)]
    output: OutputFormat,
    /// Never prompt or enter an interactive menu. Commands that need a
    /// selection require the corresponding ID or value.
    #[arg(long, global = true)]
    no_input: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Guide a new host through Pi checks, workspace authorization, pairing,
    /// and background-service setup.
    Setup {
        /// Relay WebSocket endpoint. Omit it to use LAN pairing or to answer
        /// the interactive relay prompt.
        #[arg(long, visible_alias = "relay-url", env = "PIX_RELAY_URL")]
        relay: Option<String>,
        /// Workspace root to authorize. Omit it to answer the interactive
        /// workspace prompt.
        #[arg(long, value_name = "PATH", env = "PIX_WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Friendly name for the workspace supplied with `--workspace`.
        #[arg(long, visible_alias = "name")]
        workspace_name: Option<String>,
        /// Do not start a pairing flow. Useful for preparing a host in CI.
        #[arg(long)]
        no_pair: bool,
        /// Do not install the platform user service after setup.
        #[arg(long)]
        no_service: bool,
        /// Accept setup prompts that have a safe default.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Never prompt; all required values must come from flags or the
        /// existing configuration.
        #[arg(long)]
        non_interactive: bool,
        /// Show extra local diagnostics while setup runs.
        #[arg(long)]
        verbose: bool,
        /// Expose every option instead of the recommended quick path.
        #[arg(long)]
        advanced: bool,
    },
    /// Manage explicitly authorized host folders.
    Workspace {
        #[command(subcommand)]
        command: Option<WorkspaceCommand>,
    },
    /// Manage paired iOS devices.
    Device {
        #[command(subcommand)]
        command: Option<DeviceCommand>,
    },
    /// Inspect and release active Pi runtimes owned by the host service.
    Session {
        #[command(subcommand)]
        command: Option<SessionCommand>,
    },
    /// Remember or inspect the Pi executable used by this host.
    Pi {
        #[command(subcommand)]
        command: Option<PiCommand>,
    },
    /// Configure the encrypted relay used for remote access.
    Relay {
        #[command(subcommand)]
        command: Option<RelayCommand>,
    },
    /// Show the host service log location and its most recent entries.
    Logs {
        /// Number of trailing log lines to print.
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
    /// Show configuration and host-service runtime status.
    Status,
    /// Update the pix executable from the latest GitHub release.
    Update,
    /// Install, control, and inspect the platform user service.
    Service {
        #[command(subcommand)]
        command: Option<ServiceCommand>,
    },
    /// Export a privacy-scrubbed diagnostic bundle.
    Diagnostics {
        #[command(subcommand)]
        command: DiagnosticsCommand,
    },
    /// Run the Bonjour-advertised host core until a quit command is received.
    Serve {
        /// Emit machine-readable JSONL events for a native UI bridge.
        #[arg(long)]
        json_events: bool,
        /// Run under a service manager. Lifecycle is controlled through the
        /// private local control socket instead of stdin.
        #[arg(long, hide = true)]
        service: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RelayCommand {
    /// Show the configured relay endpoint and whether it is active.
    Show,
    /// Set the relay WebSocket endpoint, e.g. `wss://relay.example.com`.
    Set { url: String },
    /// Remove the relay endpoint and stop using relay transport.
    Clear,
    /// Re-enable relay transport with the stored endpoint.
    Enable,
    /// Keep the endpoint but stop relay transport.
    Disable,
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// Authorize an existing folder on this host.
    Add {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// List authorized folders. Full paths are printed only on the host.
    List,
    /// List every native Pi session stored for one authorized folder.
    Sessions { id: uuid::Uuid },
    /// Remove an explicitly authorized folder by ID, or choose one
    /// interactively when the ID is omitted.
    Remove { id: Option<uuid::Uuid> },
}

#[derive(Debug, Subcommand)]
enum DeviceCommand {
    /// Start the host in pairing mode and follow approval prompts.
    Pair {
        /// Require a short-lived relay offer instead of local-network discovery.
        #[arg(long)]
        remote: bool,
    },
    /// List paired phones. Public keys are never printed.
    List,
    /// List pairing requests currently waiting for host approval.
    Pending,
    /// Approve one pending pairing request by request ID or six-digit code.
    Approve {
        /// Stable request ID returned by `pix device pending`.
        #[arg(long, conflicts_with = "code")]
        request: Option<uuid::Uuid>,
        /// Six-digit confirmation code shown on the phone.
        #[arg(long, conflicts_with = "request")]
        code: Option<String>,
    },
    /// Reject one pending pairing request by request ID or six-digit code.
    Reject {
        /// Stable request ID returned by `pix device pending`.
        #[arg(long, conflicts_with = "code")]
        request: Option<uuid::Uuid>,
        /// Six-digit confirmation code shown on the phone.
        #[arg(long, conflicts_with = "request")]
        code: Option<String>,
    },
    /// Revoke a paired phone by ID, or choose one interactively when omitted.
    Revoke { id: Option<String> },
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// List active Pi runtimes held by the host service.
    List,
    /// Release one active runtime so another Pi process may resume it.
    Release { id: Option<String> },
}

#[derive(Debug, Subcommand)]
enum PiCommand {
    /// Show the configured or auto-detected Pi executable.
    Show,
    /// Persist an explicit Pi executable for later `pix serve` launches.
    Set { path: PathBuf },
    /// Forget the saved Pi executable and return to PATH discovery.
    Clear,
}

#[derive(Debug, Subcommand)]
enum DiagnosticsCommand {
    /// Write a privacy-scrubbed `pix-diagnostics-*.tar.gz` bundle.
    Export {
        /// Destination file ending in `.tar.gz` or a directory to contain it.
        path: PathBuf,
    },
}

fn main() -> ExitCode {
    let requested_json = requested_json_output();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = u8::try_from(error.exit_code()).unwrap_or(2);
            if requested_json
                && !matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            {
                let _ = CommandOutput::error("usage", &error.to_string());
            } else {
                let _ = error.print();
            }
            return ExitCode::from(exit_code);
        }
    };
    let output = CommandOutput::new(cli.output);
    match run(cli, output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let usage_error = error.downcast_ref::<CliUsageError>().is_some();
            if output.is_json() {
                let _ = CommandOutput::error(
                    if usage_error {
                        "usage"
                    } else {
                        "command_failed"
                    },
                    &format!("{error:#}"),
                );
            } else {
                eprintln!("Error: {error:#}");
            }
            if usage_error {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn requested_json_output() -> bool {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let mut explicit = None;
    let mut index = 1;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy();
        if let Some(value) = argument.strip_prefix("--output=") {
            explicit = Some(value.eq_ignore_ascii_case("json"));
        } else if argument == "--output" && index + 1 < arguments.len() {
            explicit = Some(arguments[index + 1].eq_ignore_ascii_case("json"));
            index += 1;
        }
        index += 1;
    }
    explicit.unwrap_or_else(|| {
        std::env::var("PIX_OUTPUT").is_ok_and(|value| value.eq_ignore_ascii_case("json"))
    })
}

fn usage_error(message: impl Into<String>) -> anyhow::Error {
    CliUsageError::new(message).into()
}

fn run(cli: Cli, output: CommandOutput) -> Result<()> {
    let config_path = match cli.config {
        Some(path) => path,
        None => ConfigStore::default_path().context("locating Pix configuration directory")?,
    };
    let store = ConfigStore::new(config_path);

    let interactive = !cli.no_input
        && !output.is_json()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal();

    let Some(command) = cli.command else {
        if !interactive {
            if output.is_json() {
                return Err(usage_error(
                    "a command is required with JSON output; run `pix --help`",
                ));
            }
            let mut command = Cli::command();
            command.print_long_help().context("printing Pix help")?;
            println!();
            return Ok(());
        }
        let overview = HostOverview::collect(&store);
        return match home::run(&overview, SetupUi::new(true, false))? {
            HomeAction::Setup => setup(&store, &default_setup_options()),
            HomeAction::Devices => device(&store, None, output, true),
            HomeAction::Workspaces => workspace(&store, None, output, true),
            HomeAction::Status => status_command(&store, output),
            HomeAction::Settings => relay_command(&store, None, output, true),
            HomeAction::Commands => {
                let mut command = Cli::command();
                command.print_long_help().context("printing Pix help")?;
                println!();
                Ok(())
            }
            HomeAction::Quit => Ok(()),
        };
    };

    match command {
        Command::Setup {
            relay,
            workspace,
            workspace_name,
            no_pair,
            no_service,
            yes,
            non_interactive,
            advanced,
            verbose,
        } => {
            if output.is_json() {
                return Err(usage_error(
                    "the setup wizard is human-facing; use explicit `status`, `workspace`, `relay`, and `service` commands for JSON automation",
                ));
            }
            setup(
                &store,
                &SetupOptions {
                    relay,
                    workspace,
                    workspace_name,
                    no_pair,
                    no_service,
                    yes,
                    non_interactive: non_interactive || cli.no_input,
                    advanced,
                    verbose,
                },
            )
        }
        Command::Workspace { command } => workspace(&store, command, output, interactive),
        Command::Device { command } => device(&store, command, output, interactive),
        Command::Session { command } => session(&store, command, output, interactive),
        Command::Pi { command } => pi_command(&store, command, output, interactive),
        Command::Relay { command } => relay_command(&store, command, output, interactive),
        Command::Logs { tail } => show_logs(&store, tail, output),
        Command::Status => status_command(&store, output),
        Command::Update => update(&store, output),
        Command::Service { command } => service_command(&store, command, output, interactive),
        Command::Diagnostics { command } => diagnostics_command(&store, command, output),
        Command::Serve {
            json_events,
            service,
        } => {
            if output.is_json() {
                return Err(usage_error(
                    "`pix serve` is a streaming process; use `pix serve --json-events` for its JSONL event stream, without global `--output json`",
                ));
            }
            serve(&store, json_events, service)
        }
    }
}

fn print_cli_help() -> Result<()> {
    let mut command = Cli::command();
    command.print_long_help().context("printing Pix help")?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::commands::relay::validate_relay_url;
    use crate::commands::shared::format_confirmation_code;
    use crate::serve::{
        HostLog, ServeEvent, human_event, loggable_event, pairing_instructions,
        render_remote_pairing,
    };

    #[test]
    fn loggable_events_omit_paths_names_and_secrets() {
        let event = ServeEvent::Environment {
            source: "login shell (/Users/dev/.local/bin/zsh)".to_owned(),
            path_entries: 3,
            pi_executable: "/Users/dev/.local/bin/pi".to_owned(),
        };
        let rendered = loggable_event(&event).to_string();
        assert!(rendered.contains("path_entries"));
        assert!(!rendered.contains("/Users/dev"));
        assert!(!rendered.contains("pi_executable"));
    }

    #[test]
    fn loggable_events_redact_remote_pairing_material() {
        let event = ServeEvent::RemotePairingReady {
            qr_payload: "pix://pair?secret=top-secret".to_owned(),
            join_code: "ABCD-EFGH".to_owned(),
            expires_at: 123,
            local_request_id: None,
        };
        let rendered = loggable_event(&event).to_string();
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("top-secret"));
        assert!(!rendered.contains("ABCD-EFGH"));
    }

    #[test]
    fn human_events_do_not_fall_back_to_debug_structs() {
        let rendered = human_event(&ServeEvent::Ready {
            port: 1234,
            fingerprint: "fingerprint".to_owned(),
        });
        assert_eq!(rendered, "✓ Pix host is ready\n");
        assert!(!rendered.contains("Ready {"));
    }

    #[test]
    fn confirmation_codes_are_grouped_for_humans() {
        assert_eq!(format_confirmation_code("877437"), "877 437");
        assert_eq!(format_confirmation_code("12345"), "12345");
    }

    #[test]
    fn pairing_instructions_match_the_selected_transport() {
        assert!(pairing_instructions(true).contains("scan this QR code"));
        assert!(!pairing_instructions(false).contains("QR"));
        assert!(pairing_instructions(false).contains("nearby hosts"));
    }

    #[test]
    fn terminal_qr_renderer_keeps_raw_payload_out_of_human_text() {
        let rendered = render_remote_pairing(
            "pix://pair?v=1&relay=wss%3A%2F%2Fexample.test&secret=top-secret",
            "KR9M-PBYA",
            123,
        );
        assert!(rendered.contains("Scan this QR code with Pix"));
        assert!(rendered.contains("Pairing code"));
        assert!(rendered.contains("KR9M-PBYA"));
        assert!(!rendered.contains("top-secret"));
    }

    #[test]
    fn setup_relay_validation_accepts_only_websocket_endpoints() {
        assert_eq!(
            validate_relay_url(" wss://relay.example.com ").expect("valid relay"),
            "wss://relay.example.com"
        );
        assert!(validate_relay_url("https://relay.example.com").is_err());
        assert!(validate_relay_url("wss://relay.example.com/with space").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn host_log_does_not_follow_symlink_targets() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("log directory");
        let config_path = directory.path().join("config.json");
        let log_path = HostLog::path_for(&config_path);
        fs::create_dir_all(log_path.parent().expect("log parent")).expect("log parent");
        let protected = directory.path().join("protected.txt");
        fs::write(&protected, b"sentinel").expect("protected file");
        symlink(&protected, &log_path).expect("log symlink");

        let log = HostLog::open(&config_path);
        log.append_text("lifecycle", "should not reach the target");

        assert_eq!(
            fs::read(&protected).expect("protected file remains"),
            b"sentinel"
        );
    }
}
