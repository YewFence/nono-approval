use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use bytesize::ByteSize;
use clap::{Args, CommandFactory as _, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};

use crate::broker::{ApprovalId, BrokerConfig, DEFAULT_DENIAL_REASON, validate_denial_reason};
use crate::config::{ResolvedConfig, load, setup};
use crate::control::{ApprovalView, ControlClient, DebugCaptureStatus, DecisionRequest};
use crate::daemon::{DaemonConfig, run};
use crate::debug_capture::{DebugCapture, clean_captures, list_captures};
use crate::display::truncate_summary;
use crate::runtime_path::ProjectPaths;
use crate::webhook::WEBHOOK_PATH;

#[derive(Debug, Parser)]
#[command(name = "nono-approval", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the local configuration and print the nono profile fragment.
    Setup,
    /// Validate the local approval configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Run the local approval daemon.
    Serve(ServeArgs),
    /// Show the running daemon's status.
    Status(ClientArgs),
    /// List pending approval requests.
    List(ListArgs),
    /// Show one approval request by its full ID.
    Show(ShowArgs),
    /// Approve one pending request by its full ID.
    Approve(DecisionArgs),
    /// Deny one pending request by its full ID.
    Deny(DenyArgs),
    /// Inspect or remove explicit debug captures.
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },
    #[command(name = "__probe-control-socket", hide = true)]
    ProbeControlSocket(ClientArgs),
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Probe whether a nono profile blocks the control socket.
    Validate {
        /// nono profile name or path.
        #[arg(long)]
        profile: String,
        #[command(flatten)]
        client: ClientArgs,
    },
}

#[derive(Clone, Copy, Debug, Subcommand)]
enum DebugCommand {
    /// List managed debug capture files.
    Captures,
    /// Delete managed debug capture files.
    Clean,
}

#[derive(Clone, Debug, Args)]
struct ClientArgs {
    #[arg(long, hide = true)]
    control_socket: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[command(flatten)]
    client: ClientArgs,
    /// Print pending approvals as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ShowArgs {
    /// Full approval ID, such as `appr_0123456789abcdef`.
    approval_id: ApprovalId,
    /// Include wire-level debug metadata.
    #[arg(long)]
    debug: bool,
    #[command(flatten)]
    client: ClientArgs,
}

#[derive(Debug, Args)]
struct DecisionArgs {
    /// Full approval ID, such as `appr_0123456789abcdef`.
    approval_id: ApprovalId,
    #[command(flatten)]
    client: ClientArgs,
}

#[derive(Debug, Args)]
struct DenyArgs {
    /// Full approval ID, such as `appr_0123456789abcdef`.
    approval_id: ApprovalId,
    /// Reason returned with the denial.
    #[arg(long)]
    reason: Option<String>,
    #[command(flatten)]
    client: ClientArgs,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LogFormat {
    /// Human-readable log lines.
    Text,
    /// Structured JSON log records.
    Json,
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Webhook listener address.
    #[arg(long)]
    webhook_listen: Option<SocketAddr>,
    #[arg(long)]
    control_socket: Option<PathBuf>,
    /// Approval lease duration.
    #[arg(long, value_parser = parse_duration)]
    request_timeout: Option<Duration>,
    /// Maximum number of pending requests.
    #[arg(long)]
    max_pending: Option<usize>,
    /// Maximum pending requests from one session.
    #[arg(long)]
    max_per_session: Option<usize>,
    /// Maximum webhook request body size.
    #[arg(long, value_parser = parse_byte_size)]
    max_body: Option<usize>,
    /// Persist explicit debug capture events for this daemon run.
    #[arg(long)]
    debug_capture: bool,
    /// Log output format.
    #[arg(long, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    humantime::parse_duration(value).map_err(|error| error.to_string())
}

fn parse_byte_size(value: &str) -> Result<usize, String> {
    let size = ByteSize::from_str(value)?;
    usize::try_from(size.as_u64()).map_err(|_| "byte size does not fit this platform".to_owned())
}

/// Parses command-line arguments and executes one command.
///
/// # Errors
///
/// Returns an error when configuration, transport, validation, or daemon startup fails.
pub async fn run_cli() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    execute(cli).await
}

async fn execute(cli: Cli) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Some(Command::Setup) => setup_command()?,
        Some(Command::Config {
            command: ConfigCommand::Validate { profile, client },
        }) => validate_config(&profile, client).await?,
        Some(Command::Serve(args)) => serve(args).await?,
        Some(Command::Status(args)) => status(args).await?,
        Some(Command::List(args)) => list(args).await?,
        Some(Command::Show(args)) => show(args).await?,
        Some(Command::Approve(args)) => decide(args, DecisionRequest::Granted).await?,
        Some(Command::Deny(args)) => {
            let reason = args
                .reason
                .unwrap_or_else(|| DEFAULT_DENIAL_REASON.to_owned());
            validate_denial_reason(&reason)?;
            decide(
                DecisionArgs {
                    approval_id: args.approval_id,
                    client: args.client,
                },
                DecisionRequest::Denied { reason },
            )
            .await?;
        }
        Some(Command::Debug { command }) => debug_command(command)?,
        Some(Command::Completions { shell }) => {
            generate(
                shell,
                &mut Cli::command(),
                "nono-approval",
                &mut std::io::stdout(),
            );
        }
        Some(Command::ProbeControlSocket(args)) => {
            let path = client_path(args)?;
            crate::profile_validation::run_probe(&path).await?;
        }
        None => {
            let paths = ProjectPaths::resolve()?;
            crate::interactive::run(&paths.control_socket).await?;
        }
    }
    Ok(())
}

async fn serve(args: ServeArgs) -> Result<(), Box<dyn Error>> {
    let paths = ProjectPaths::resolve()?;
    let file = load(&paths.config_file)?;
    let resolved = merge_config(file.resolve()?, &args);
    validate_resolved(&resolved)?;
    let control_socket = args.control_socket.unwrap_or(paths.control_socket);
    let mut config = DaemonConfig::new(resolved.webhook_listen, control_socket);
    config.broker = BrokerConfig {
        request_timeout: resolved.request_timeout,
        max_pending: resolved.max_pending,
        max_per_session: resolved.max_per_session,
        ..BrokerConfig::default()
    };
    config.max_body_bytes = resolved.max_body_bytes;
    if args.debug_capture {
        config.debug_capture = Some(DebugCapture::create(&paths.state_dir)?);
    }
    init_logging(args.log_format)?;
    run(config).await?;
    Ok(())
}

async fn status(args: ClientArgs) -> Result<(), Box<dyn Error>> {
    let status = client(args)?.status().await?;
    println!("Daemon: running");
    println!("Pending: {}", status.pending);
    println!("Started: {}s ago", status.uptime_seconds);
    println!("Webhook: {}", status.webhook_listen);
    match status.debug_capture {
        DebugCaptureStatus::Disabled => println!("Debug capture: disabled"),
        DebugCaptureStatus::Enabled { path } => {
            println!(
                "Debug capture: enabled ({})",
                crate::display::sanitize(&path.display().to_string())
            );
        }
        DebugCaptureStatus::Failed { error_category } => {
            println!("Debug capture: failed ({error_category})");
        }
    }
    Ok(())
}

async fn list(args: ListArgs) -> Result<(), Box<dyn Error>> {
    let approvals = client(args.client)?.list().await?.approvals;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&approvals)?);
        return Ok(());
    }
    println!("ID                     TYPE        REQUEST");
    let terminal_width = crossterm::terminal::size().map_or(80, |(width, _)| usize::from(width));
    let summary_width = terminal_width.saturating_sub(34);
    for approval in approvals {
        println!(
            "{:<21} {:<11} {}",
            approval.approval_id,
            approval.capability_type,
            truncate_summary(&approval.summary, summary_width)
        );
    }
    Ok(())
}

async fn show(args: ShowArgs) -> Result<(), Box<dyn Error>> {
    match client(args.client)?
        .show(&args.approval_id, args.debug)
        .await?
    {
        ApprovalView::Pending(detail) => {
            println!("Approval: {}", detail.approval_id);
            for field in detail.content.fields {
                println!("{}: {}", field.label, field.value);
            }
            println!("Received: {}", detail.received_at);
            println!("Deadline: {}", detail.deadline);
            if args.debug {
                println!();
                println!("Debug:");
                if let Some(debug) = detail.debug {
                    println!(
                        "Claimed backend: {}",
                        crate::display::sanitize(&debug.claimed_backend)
                    );
                    println!("Source: {:?}", debug.source_kind);
                    println!("Wire DTO: {}", serde_json::to_string(&debug.wire_request)?);
                }
            }
        }
        ApprovalView::Completed(completed) => {
            println!("Approval: {}", completed.approval_id);
            println!("State: {:?}", completed.state);
            println!("Completed: {}", completed.completed_at);
        }
    }
    Ok(())
}

async fn decide(args: DecisionArgs, decision: DecisionRequest) -> Result<(), Box<dyn Error>> {
    let response = client(args.client)?
        .decide(&args.approval_id, &decision)
        .await?;
    println!("{}: {:?}", response.approval_id, response.state);
    Ok(())
}

fn client(args: ClientArgs) -> Result<ControlClient, Box<dyn Error>> {
    Ok(ControlClient::new(client_path(args)?))
}

fn client_path(args: ClientArgs) -> Result<PathBuf, Box<dyn Error>> {
    Ok(match args.control_socket {
        Some(path) => path,
        None => ProjectPaths::resolve()?.control_socket,
    })
}

fn setup_command() -> Result<(), Box<dyn Error>> {
    let paths = ProjectPaths::resolve()?;
    let config = setup(&paths.config_file)?;
    let resolved = config.resolve()?;
    let endpoint = format!("http://{}{}", resolved.webhook_listen, WEBHOOK_PATH);
    println!(
        "Configuration: {}",
        crate::display::sanitize(&paths.config_file.display().to_string())
    );
    println!("Webhook endpoint: {endpoint}");
    println!();
    println!("nono profile fragment:");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "command_policies": {
                "approval_backends": {
                    "local-broker": {
                        "type": "webhook",
                        "url": endpoint,
                        "timeout_secs": 300
                    }
                },
                "approval_defaults": {
                    "backend": "local-broker",
                    "timeout_secs": 300
                }
            }
        }))?
    );
    println!();
    println!("Validate isolation with: nono-approval config validate --profile <name-or-path>");
    Ok(())
}

async fn validate_config(profile: &str, args: ClientArgs) -> Result<(), Box<dyn Error>> {
    eprintln!("warning: nono may run this profile's host-side session hooks during validation");
    crate::profile_validation::validate_profile(profile, &client_path(args)?).await?;
    println!("Control socket access was denied by the sandbox (EACCES/EPERM).");
    Ok(())
}

fn merge_config(mut config: ResolvedConfig, args: &ServeArgs) -> ResolvedConfig {
    config.webhook_listen = args.webhook_listen.unwrap_or(config.webhook_listen);
    config.request_timeout = args.request_timeout.unwrap_or(config.request_timeout);
    config.max_pending = args.max_pending.unwrap_or(config.max_pending);
    config.max_per_session = args.max_per_session.unwrap_or(config.max_per_session);
    config.max_body_bytes = args.max_body.unwrap_or(config.max_body_bytes);
    config
}

fn validate_resolved(config: &ResolvedConfig) -> Result<(), Box<dyn Error>> {
    if !config.webhook_listen.ip().is_loopback() {
        return Err("webhook listener must use a loopback IP address".into());
    }
    if config.max_pending == 0 || config.max_per_session == 0 || config.max_body_bytes == 0 {
        return Err("configured limits must be greater than zero".into());
    }
    if config.max_per_session > config.max_pending {
        return Err("max-per-session must not exceed max-pending".into());
    }
    Ok(())
}

fn debug_command(command: DebugCommand) -> Result<(), Box<dyn Error>> {
    let paths = ProjectPaths::resolve()?;
    match command {
        DebugCommand::Captures => {
            for capture in list_captures(&paths.state_dir)? {
                println!(
                    "{}\t{}\t{} bytes",
                    capture.name, capture.created_at, capture.size_bytes
                );
            }
        }
        DebugCommand::Clean => {
            let count = clean_captures(&paths.state_dir)?;
            println!("Deleted {count} debug capture file(s).");
        }
    }
    Ok(())
}

fn init_logging(format: LogFormat) -> Result<(), Box<dyn Error>> {
    match format {
        LogFormat::Text => tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|error| format!("failed to initialize logging: {error}"))?,
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|error| format!("failed to initialize logging: {error}"))?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::Cli;

    #[test]
    fn rejects_partial_approval_id_at_parse_time() {
        assert!(Cli::try_parse_from(["nono-approval", "approve", "appr_1234"]).is_err());
    }

    #[test]
    fn public_subcommands_have_help_descriptions() {
        let command = Cli::command();
        for name in [
            "setup",
            "config",
            "serve",
            "status",
            "list",
            "show",
            "approve",
            "deny",
            "debug",
            "completions",
        ] {
            let subcommand = command
                .get_subcommands()
                .find(|subcommand| subcommand.get_name() == name)
                .unwrap_or_else(|| panic!("missing subcommand {name}"));
            let description = subcommand.get_about().map(ToString::to_string);
            assert!(
                description.is_some_and(|description| !description.is_empty()),
                "missing help description for {name}"
            );
        }
    }
}
