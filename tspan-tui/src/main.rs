use clap::Parser;

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

    /// Print API requests, status codes, and raw response bodies to stderr
    ///
    /// Raw bodies may contain API tokens, commands, and other sensitive data.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    app::run(app::TuiOptions {
        server_url: cli.server,
        username: cli.username,
        password: cli.password,
        initial_client_id: cli.client_id,
        timezone: cli.timezone,
        page_size: cli.page_size,
        verbose: cli.verbose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_short_and_long_flags_are_accepted() {
        assert!(Cli::try_parse_from(["tspan-tui", "-v"]).unwrap().verbose);
        assert!(
            Cli::try_parse_from(["tspan-tui", "--verbose"])
                .unwrap()
                .verbose
        );
    }
}
