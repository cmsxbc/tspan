use std::io;
use std::path::Path;

use base64::Engine;
use clap::Parser;

#[derive(Parser)]
#[command(name = "tspan-backup")]
#[command(about = "Download a database backup from a tspan-server instance")]
struct Cli {
    #[arg(short, long, env = "BACKUP_URL")]
    url: String,

    #[arg(short, long, default_value = "admin", env = "BACKUP_USER")]
    user: String,

    #[arg(short, long, default_value = "changeme", env = "BACKUP_PASSWORD")]
    password: String,

    #[arg(short, long, env = "BACKUP_OUTPUT")]
    output: String,

    /// Keep only the N most recent backups (0 = keep all)
    #[arg(long, default_value = "7", env = "BACKUP_KEEP")]
    keep: usize,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let output_path = resolve_output_path(&cli.output)?;

    let auth = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", cli.user, cli.password));

    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(120)))
            .build(),
    );

    let response = agent
        .get(&cli.url)
        .header("Authorization", &format!("Basic {auth}"))
        .call()?;

    if response.status() != 200 {
        anyhow::bail!(
            "Backup request failed with status {}",
            response.status()
        );
    }

    let mut body = response.into_body();
    let mut reader = body.as_reader();
    let mut file = std::fs::File::create(&output_path)?;
    io::copy(&mut reader, &mut file)?;

    let size = std::fs::metadata(&output_path)?.len();
    println!("Backup saved to {} ({} bytes)", output_path.display(), size);

    if cli.keep > 0 {
        if let Some(dir) = output_path.parent() {
            cleanup_old_backups(dir, cli.keep)?;
        }
    }

    Ok(())
}

fn resolve_output_path(output: &str) -> anyhow::Result<std::path::PathBuf> {
    let path = Path::new(output);

    // If output ends with '/' or is an existing directory, generate a timestamped filename inside it
    if output.ends_with('/') || (path.exists() && path.is_dir()) {
        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let filename = format!("tspan-backup-{}.db", timestamp);
        return Ok(path.join(filename));
    }

    // Otherwise use the path as-is (parent directory must exist)
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }

    Ok(path.to_path_buf())
}

fn cleanup_old_backups(dir: &Path, keep: usize) -> anyhow::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("tspan-backup-") && n.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect();

    if entries.len() <= keep {
        return Ok(());
    }

    // Sort by modified time descending (newest first)
    entries.sort_by(|a, b| {
        let ta = a.metadata().and_then(|m| m.modified()).ok();
        let tb = b.metadata().and_then(|m| m.modified()).ok();
        tb.cmp(&ta)
    });

    for entry in entries.iter().skip(keep) {
        let path = entry.path();
        if let Err(e) = std::fs::remove_file(&path) {
            eprintln!("Warning: failed to remove old backup {}: {}", path.display(), e);
        } else {
            println!("Removed old backup {}", path.display());
        }
    }

    Ok(())
}
