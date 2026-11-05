use plamo_translate_mcp::PlamoServer;
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // stdout carries the MCP transport, so diagnostics must stay on stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_env("PLAMO_TRANSLATE_MCP_LOG")
                .unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let server = match PlamoServer::from_env() {
        Ok(server) => server,
        Err(e) => {
            eprintln!("plamo-translate-mcp: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    let running = match server.serve(stdio()).await {
        Ok(running) => running,
        Err(e) => {
            eprintln!("plamo-translate-mcp: failed to start MCP session: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Err(e) = running.waiting().await {
        eprintln!("plamo-translate-mcp: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
