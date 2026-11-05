mod daemon;
mod translate;

use clap::{Parser, Subcommand};
use plamo_client::{Client, Config};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Ok = 0,
    Translation = 1,
    Usage = 2,
    Unreachable = 3,
}

/// SIGINT exits with 130, deliberately outside `ExitCode`.
pub const EXIT_INTERRUPTED: i32 = 130;

// args_conflicts_with_subcommands lets `plamo-translate hello` translate "hello" while
// `plamo-translate status` runs the subcommand; the cost is that text equal to a
// subcommand name must be piped via stdin.
#[derive(Parser)]
#[command(
    name = "plamo-translate",
    version,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(flatten)]
    translate: translate::TranslateArgs,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show the daemon status
    Status,
    /// Manage the model
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Manage the launchd daemon
    Daemon {
        #[command(subcommand)]
        action: daemon::DaemonCmd,
    },
}

#[derive(Subcommand)]
enum ModelAction {
    /// Download and load the model now (op=load)
    Pull,
    /// Print where the model weights are stored
    Path,
}

pub(crate) fn client_for(config: &Config) -> Client {
    let client = Client::new(config.socket_path.clone());
    // Test-only override: a missing socket is retried for the full 5 s otherwise.
    match std::env::var("PLAMO_TRANSLATE_CONNECT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(ms) => client.with_connect_timeout(Duration::from_millis(ms)),
        None => client,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(ExitCode::Usage as i32);
        }
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let client = client_for(&config);
    let code = match cli.command {
        None => translate::run(&client, &config, cli.translate).await,
        Some(Command::Status) => translate::status(&client).await,
        Some(Command::Model {
            action: ModelAction::Pull,
        }) => translate::pull(&client).await,
        Some(Command::Model {
            action: ModelAction::Path,
        }) => {
            println!("{}", daemon::hub_dir(&home).display());
            ExitCode::Ok as i32
        }
        Some(Command::Daemon { action }) => {
            match daemon::run(action, &config, &daemon::launchctl::SystemLaunchctl, &home).await {
                Ok(code) => code as i32,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::Translation as i32
                }
            }
        }
    };
    std::process::exit(code);
}
