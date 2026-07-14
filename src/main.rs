use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod db;
mod importer;
mod markdown;
mod server;
mod stats;
mod svg_calendar;
mod tui;

use auth::AuthConfig;
use server::AppState;

#[derive(Parser)]
#[command(name = "tspan-server", version)]
#[command(about = "What You Are Doing - Activity Tracker Server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, default_value = "data.db", env = "DATABASE_URL")]
    database: String,

    #[arg(short, long, default_value = "0.0.0.0:8080")]
    bind: String,

    #[arg(long, default_value = "admin")]
    web_username: String,

    #[arg(long, default_value = "changeme", env = "WEB_PASSWORD")]
    web_password: String,

    #[arg(long, default_value = "5")]
    command_token_limit: usize,
}

#[derive(Subcommand)]
enum Commands {
    /// Import historical records from a directory
    Import {
        #[arg(long, default_value = "imported")]
        client_id: String,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        alias: Option<String>,
        path: String,
    },
    /// Generate a new API token
    TokenGenerate {
        #[arg(long, default_value = "default")]
        client_id: String,
        description: Option<String>,
    },
    /// List all API tokens
    TokenList,
    /// Revoke an API token
    TokenRevoke {
        token: String,
    },
    /// Open the interactive terminal admin interface
    Tui {
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
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tspan_server=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    // The TUI is a remote API client and must not open the local database.
    if let Some(Commands::Tui {
        server,
        username,
        password,
        client_id,
        timezone,
        page_size,
    }) = &cli.command
    {
        return tui::run(tui::TuiOptions {
            server_url: server.clone(),
            username: username.clone(),
            password: password.clone(),
            initial_client_id: client_id.clone(),
            timezone: timezone.clone(),
            page_size: *page_size,
        });
    }

    let pool = db::create_pool(&cli.database)?;

    match cli.command {
        Some(Commands::Import { client_id, command, alias, path }) => {
            println!("Importing from {} as client '{}'...", path, client_id);
            let result = importer::import_from_directory(&pool, &client_id, &path, command.as_deref(), alias.as_deref()).await?;
            println!("Imported: {}, Failed: {}", result.imported, result.failed);
            for err in &result.errors {
                eprintln!("  ERROR: {}", err);
            }
            Ok(())
        }
        Some(Commands::TokenGenerate { client_id, description }) => {
            let token = format!("tspan_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
            let mut conn = pool.lock();
            db::add_api_token(&mut conn, &token, &client_id, description.as_deref())?;
            println!("Generated token: {}", token);
            println!("Client ID: {}", client_id);
            Ok(())
        }
        Some(Commands::TokenList) => {
            let mut conn = pool.lock();
            let tokens = db::list_api_tokens(&mut conn)?;
            for t in tokens {
                println!("{} - {} - {}", t.token, t.description.unwrap_or_default(), t.created_at);
            }
            Ok(())
        }
        Some(Commands::TokenRevoke { token }) => {
            let mut conn = pool.lock();
            if db::delete_api_token(&mut conn, &token)? {
                println!("Token revoked.");
            } else {
                println!("Token not found.");
            }
            Ok(())
        }
        Some(Commands::Tui { .. }) => unreachable!("TUI handled before database initialization"),
        None => {
            // Default: start server
            // Ensure at least one token exists for initial setup
            let auth = AuthConfig {
                web_username: cli.web_username.clone(),
                web_password_hash: cli.web_password.clone(),
            };

            let state = AppState { pool, auth, command_token_limit: cli.command_token_limit };

            // Ensure at least one token exists for initial setup
            {
                let mut conn = state.pool.lock();
                let tokens = db::list_api_tokens(&mut conn)?;
                if tokens.is_empty() {
                    let token = format!("tspan_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
                    db::add_api_token(&mut conn, &token, "default", Some("auto-generated initial token"))?;
                    println!("WARNING: No API tokens found. Auto-generated initial token: {}", token);
                    println!("Set TSPANRUN_TOKEN environment variable to this value.");
                }
            }

            // Background maintenance: WAL checkpoint every hour, integrity check every 6 hours
            let bg_pool = state.pool.clone();
            tokio::spawn(async move {
                let mut tick_count = 0u64;
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
                loop {
                    interval.tick().await;
                    tick_count += 1;
                    let mut conn = bg_pool.lock();
                    if let Err(e) = db::run_wal_checkpoint(&mut conn) {
                        tracing::warn!("WAL checkpoint failed: {}", e);
                    }
                    if tick_count % 6 == 0 {
                        match db::check_integrity(&mut conn) {
                            Ok(ref result) if result == "ok" => {
                                tracing::info!("Database integrity check passed");
                            }
                            Ok(result) => {
                                tracing::error!("Database integrity check FAILED: {}", result);
                            }
                            Err(e) => {
                                tracing::error!("Integrity check error: {}", e);
                            }
                        }
                    }
                }
            });

            let app = server::create_router(state);

            let listener = tokio::net::TcpListener::bind(&cli.bind).await?;
            let access_url = if cli.bind.starts_with("0.0.0.0:") {
                cli.bind.replacen("0.0.0.0", "127.0.0.1", 1)
            } else if cli.bind.starts_with("[::]:") {
                cli.bind.replacen("[::]", "[::1]", 1)
            } else {
                cli.bind.clone()
            };
            println!("Server listening on http://{}", cli.bind);
            println!("  → Web UI: http://{}/", access_url);
            println!("  → Admin:  http://{}/admin", access_url);
            axum::serve(listener, app).await?;
            Ok(())
        }
    }
}
