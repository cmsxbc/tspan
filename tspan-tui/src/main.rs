use clap::Parser;
use std::path::PathBuf;

mod api_types;
mod app;

#[derive(Parser)]
#[command(name = "tspan-tui", version)]
#[command(about = "Remote terminal administration client for tspan-server")]
struct Cli {
    /// Base URL of the remote tspan server
    #[arg(
        long,
        default_value = "http://127.0.0.1:8080",
        env = "TSPAN_TUI_SERVER"
    )]
    server: String,

    /// HTTP Basic Auth username
    #[arg(long, default_value = "admin", env = "TSPAN_TUI_USERNAME")]
    username: String,

    /// HTTP Basic Auth password
    #[arg(long, default_value = "changeme", env = "TSPAN_TUI_PASSWORD")]
    password: String,

    /// Initially selected client (defaults to all clients)
    #[arg(long, default_value = "__global__")]
    client_id: String,

    /// Time zone used to display timestamps and compute daily statistics
    #[arg(long, default_value = "UTC")]
    timezone: String,

    /// Number of records shown on each page
    #[arg(
        long,
        default_value_t = 25,
        value_parser = clap::value_parser!(u16).range(5..=200)
    )]
    page_size: u16,

    /// Write API requests, status codes, and raw response bodies to a log file
    ///
    /// Raw bodies may contain API tokens, commands, and other sensitive data.
    #[arg(short, long)]
    verbose: bool,

    /// Verbose API log path; also enables verbose logging
    #[arg(long, value_name = "PATH", env = "TSPAN_TUI_VERBOSE_LOG")]
    verbose_log: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let verbose_log = resolve_verbose_log(cli.verbose, cli.verbose_log);
    if let Some(path) = verbose_log.as_ref() {
        eprintln!("tspan-tui: verbose API log: {}", path.display());
    }
    app::run(app::TuiOptions {
        server_url: cli.server,
        username: cli.username,
        password: cli.password,
        initial_client_id: cli.client_id,
        timezone: cli.timezone,
        page_size: cli.page_size,
        verbose_log,
    })
}

fn resolve_verbose_log(verbose: bool, path: Option<PathBuf>) -> Option<PathBuf> {
    path.or_else(|| verbose.then(|| PathBuf::from("tspan-tui-api.log")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_short_and_long_flags_are_accepted() {
        let cli = Cli::try_parse_from(["tspan-tui", "-v"]).unwrap();
        assert!(cli.verbose);
        assert_eq!(
            resolve_verbose_log(cli.verbose, cli.verbose_log),
            Some(PathBuf::from("tspan-tui-api.log"))
        );
        assert!(
            Cli::try_parse_from(["tspan-tui", "--verbose"])
                .unwrap()
                .verbose
        );
        let cli = Cli::try_parse_from(["tspan-tui", "--verbose-log", "trace.log"]).unwrap();
        assert_eq!(
            resolve_verbose_log(cli.verbose, cli.verbose_log),
            Some(PathBuf::from("trace.log"))
        );
    }
}
