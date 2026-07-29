use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use bytesize::ByteSize;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::broker::{ApprovalId, BrokerConfig, DEFAULT_DENIAL_REASON, validate_denial_reason};
use crate::control::{ApprovalView, ControlClient, DebugCaptureStatus, DecisionRequest};
use crate::daemon::{DaemonConfig, run};
use crate::display::truncate_summary;
use crate::runtime_path::ProjectPaths;

#[derive(Debug, Parser)]
#[command(name = "nono-approval", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve(ServeArgs),
    Status(ClientArgs),
    List(ListArgs),
    Show(ShowArgs),
    Approve(DecisionArgs),
    Deny(DenyArgs),
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
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ShowArgs {
    approval_id: ApprovalId,
    #[arg(long)]
    debug: bool,
    #[command(flatten)]
    client: ClientArgs,
}

#[derive(Debug, Args)]
struct DecisionArgs {
    approval_id: ApprovalId,
    #[command(flatten)]
    client: ClientArgs,
}

#[derive(Debug, Args)]
struct DenyArgs {
    approval_id: ApprovalId,
    #[arg(long)]
    reason: Option<String>,
    #[command(flatten)]
    client: ClientArgs,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:17443")]
    webhook_listen: SocketAddr,
    #[arg(long)]
    control_socket: Option<PathBuf>,
    #[arg(long, default_value = "270s", value_parser = parse_duration)]
    request_timeout: Duration,
    #[arg(long, default_value_t = 64)]
    max_pending: usize,
    #[arg(long, default_value_t = 8)]
    max_per_session: usize,
    #[arg(long, default_value = "256KiB", value_parser = parse_byte_size)]
    max_body: usize,
    #[arg(long)]
    debug_capture: bool,
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
        None => {
            list(ListArgs {
                client: ClientArgs {
                    control_socket: None,
                },
                json: false,
            })
            .await?;
        }
    }
    Ok(())
}

async fn serve(args: ServeArgs) -> Result<(), Box<dyn Error>> {
    if args.max_pending == 0 || args.max_per_session == 0 {
        return Err("pending limits must be greater than zero".into());
    }
    if args.max_per_session > args.max_pending {
        return Err("max-per-session must not exceed max-pending".into());
    }
    let paths = ProjectPaths::resolve()?;
    let control_socket = args.control_socket.unwrap_or(paths.control_socket);
    let mut config = DaemonConfig::new(args.webhook_listen, control_socket);
    config.broker = BrokerConfig {
        request_timeout: args.request_timeout,
        max_pending: args.max_pending,
        max_per_session: args.max_per_session,
        ..BrokerConfig::default()
    };
    config.max_body_bytes = args.max_body;
    if args.debug_capture {
        return Err(
            "debug capture is not available until its managed state file is initialized".into(),
        );
    }
    let _ = args.log_format;
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
            println!("Debug capture: enabled ({})", path.display());
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
    for approval in approvals {
        println!(
            "{:<21} {:<11} {}",
            approval.approval_id,
            approval.capability_type,
            truncate_summary(&approval.summary, 60)
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
                println!("Source: {:?}", detail.content.source_kind);
                for field in detail.content.debug_fields {
                    println!("{}: {}", field.label, field.value);
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
    let path = match args.control_socket {
        Some(path) => path,
        None => ProjectPaths::resolve()?.control_socket,
    };
    Ok(ControlClient::new(path))
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::Cli;

    #[test]
    fn rejects_partial_approval_id_at_parse_time() {
        assert!(Cli::try_parse_from(["nono-approval", "approve", "appr_1234"]).is_err());
    }
}
