use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod db;
mod importer;
mod markdown;
mod server;
mod stats;
mod svg_calendar;

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
    let pool = db::create_pool(&cli.database)?;

    match cli.command {
        Some(Commands::Import { client_id, path }) => {
            println!("Importing from {} as client '{}'...", path, client_id);
            let result = importer::import_from_directory(&pool, &client_id, &path).await?;
            println!("Imported: {}, Failed: {}", result.imported, result.failed);
            for err in &result.errors {
                eprintln!("  ERROR: {}", err);
            }
            Ok(())
        }
        Some(Commands::TokenGenerate { client_id, description }) => {
            let token = format!("tspan_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
            let mut conn = pool.lock().unwrap();
            db::add_api_token(&mut conn, &token, &client_id, description.as_deref())?;
            println!("Generated token: {}", token);
            println!("Client ID: {}", client_id);
            Ok(())
        }
        Some(Commands::TokenList) => {
            let mut conn = pool.lock().unwrap();
            let tokens = db::list_api_tokens(&mut conn)?;
            for t in tokens {
                println!("{} - {} - {}", t.token, t.description.unwrap_or_default(), t.created_at);
            }
            Ok(())
        }
        Some(Commands::TokenRevoke { token }) => {
            let mut conn = pool.lock().unwrap();
            if db::delete_api_token(&mut conn, &token)? {
                println!("Token revoked.");
            } else {
                println!("Token not found.");
            }
            Ok(())
        }
        None => {
            // Default: start server
            // Ensure at least one token exists for initial setup
            {
                let mut conn = pool.lock().unwrap();
                let tokens = db::list_api_tokens(&mut conn)?;
                if tokens.is_empty() {
                    let token = format!("tspan_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
                    db::add_api_token(&mut conn, &token, "default", Some("auto-generated initial token"))?;
                    println!("WARNING: No API tokens found. Auto-generated initial token: {}", token);
                    println!("Set TSPANRUN_TOKEN environment variable to this value.");
                }
            }

            let auth = AuthConfig {
                web_username: cli.web_username.clone(),
                web_password_hash: cli.web_password.clone(),
            };

            let state = AppState { pool, auth, command_token_limit: cli.command_token_limit };
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
