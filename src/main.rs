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
#[command(name = "wyd-server")]
#[command(about = "What You Are Doing - Activity Tracker Server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, default_value = "data.db")]
    database: String,

    #[arg(short, long, default_value = "0.0.0.0:8080")]
    bind: String,

    #[arg(long, default_value = "admin")]
    web_username: String,

    #[arg(long, default_value = "changeme")]
    web_password: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Import historical records from a directory
    Import {
        path: String,
    },
    /// Generate a new API token
    TokenGenerate {
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
                .unwrap_or_else(|_| "wyd_server=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let pool = db::create_pool(&cli.database)?;

    match cli.command {
        Some(Commands::Import { path }) => {
            println!("Importing from {}...", path);
            let result = importer::import_from_directory(&pool, &path).await?;
            println!("Imported: {}, Failed: {}", result.imported, result.failed);
            for err in &result.errors {
                eprintln!("  ERROR: {}", err);
            }
            Ok(())
        }
        Some(Commands::TokenGenerate { description }) => {
            let token = format!("wyd_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
            let mut conn = pool.lock().unwrap();
            db::add_api_token(&mut conn, &token, description.as_deref())?;
            println!("Generated token: {}", token);
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
                    let token = format!("wyd_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
                    db::add_api_token(&mut conn, &token, Some("auto-generated initial token"))?;
                    println!("WARNING: No API tokens found. Auto-generated initial token: {}", token);
                    println!("Set WYDRUN_TOKEN environment variable to this value.");
                }
            }

            let auth = AuthConfig {
                web_username: cli.web_username.clone(),
                web_password_hash: cli.web_password.clone(),
            };

            let state = AppState { pool, auth };
            let app = server::create_router(state);

            let listener = tokio::net::TcpListener::bind(&cli.bind).await?;
            println!("Server listening on http://{}", cli.bind);
            axum::serve(listener, app).await?;
            Ok(())
        }
    }
}
