use anyhow::Result;
use clap::Parser;
use tokio::signal;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod buffer;
mod config;
mod ebpf;
mod event;
mod exporter;
mod filter;
mod tracker;

use buffer::{RetryBuffer, RetryItem};
use config::{ClientIdMode, Config};
use ebpf::{load_and_attach, poll_ring_buffer, EbpfEvent};
use event::build_alias_and_command;
use exporter::Exporter;
use filter::Filter;
use tracker::Tracker;

fn uid_to_username(uid: u32) -> String {
    unsafe {
        let pw = libc::getpwuid(uid);
        if !pw.is_null() {
            let name = std::ffi::CStr::from_ptr((*pw).pw_name);
            return name.to_string_lossy().into_owned();
        }
    }
    uid.to_string()
}

fn build_client_id(mode: &ClientIdMode, base: &str, uid: u32) -> String {
    match mode {
        ClientIdMode::Hostname => base.to_string(),
        ClientIdMode::HostnameUid => format!("{}-{}", base, uid),
        ClientIdMode::HostnameUser => {
            let user = uid_to_username(uid);
            format!("{}-{}", base, user)
        }
        ClientIdMode::HostnameUidUser => {
            let user = uid_to_username(uid);
            format!("{}-{}-{}", base, uid, user)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tspan_ebpf=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::parse();
    tracing::info!("Starting tspan-ebpf daemon");
    tracing::info!("Server: {}", config.server);
    tracing::info!("Base client ID: {}", config.client_id);
    tracing::info!("Client ID mode: {:?}", config.client_id_mode);

    let ebpf = load_and_attach()?;
    tracing::info!("eBPF programs loaded and attached");

    let retry_buffer = RetryBuffer::new(&config.retry_file)?;
    let exporter = Exporter::new(config.server.clone(), config.token.clone());

    // Replay buffered events on startup
    match retry_buffer.replay(&exporter).await {
        Ok(n) if n > 0 => tracing::info!("Replayed {} buffered events", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("Failed to replay buffer: {}", e),
    }

    let tracker = Tracker::new();
    let filter = Filter::new(config.allow_uids.clone(), config.deny_comm.clone())?;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<EbpfEvent>(1024);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Ring buffer poll task
    let poll_handle = tokio::spawn(async move {
        if let Err(e) = poll_ring_buffer(ebpf, tx, shutdown_rx).await {
            tracing::error!("Ring buffer poll error: {}", e);
        }
    });

    // Graceful shutdown handler
    let shutdown_tx2 = shutdown_tx.clone();
    tokio::spawn(async move {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())?;
        let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt())?;
        tokio::select! {
            _ = sigterm.recv() => {},
            _ = sigint.recv() => {},
        }
        tracing::info!("Shutdown signal received");
        let _ = shutdown_tx2.send(true);
        Ok::<(), anyhow::Error>(())
    });

    // Main event processing loop
    while let Some(event) = rx.recv().await {
        match event {
            EbpfEvent::Success(data) => {
                let (alias, command) = build_alias_and_command(&data.filename, data.argc, &data.args);
                if !filter.allow(data.uid, &command) {
                    continue;
                }
                let client_id = build_client_id(&config.client_id_mode, &config.client_id, data.uid);
                let start_time = (data.start_ns / 1_000_000_000) as i64;
                match exporter
                    .start_session(&client_id, &command, &alias, data.pid, start_time)
                    .await
                {
                    Ok(session_id) => {
                        tracker.insert(data.pid, session_id, start_time, client_id.clone());
                        tracing::debug!(
                            pid = data.pid,
                            session_id = session_id,
                            client_id = %client_id,
                            alias = %alias,
                            command = %command,
                            "session started"
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to start session: {}", e);
                        let item = RetryItem::StartSession {
                            client_id,
                            command: command.clone(),
                            alias: alias.clone(),
                            process_id: data.pid,
                            timestamp: start_time,
                        };
                        if let Err(e2) = retry_buffer.append(&item) {
                            tracing::error!("Failed to buffer start session: {}", e2);
                        }
                    }
                }
            }
            EbpfEvent::Failed(data) => {
                let (alias, command) = build_alias_and_command(&data.filename, data.argc, &data.args);
                if !filter.allow(data.uid, &command) {
                    continue;
                }
                let client_id = build_client_id(&config.client_id_mode, &config.client_id, data.uid);
                let timestamp = (data.start_ns / 1_000_000_000) as i64;
                match exporter
                    .log_failed(&client_id, &command, &alias, data.pid, timestamp, data.errno)
                    .await
                {
                    Ok(record_id) => {
                        tracing::debug!(
                            pid = data.pid,
                            record_id = record_id,
                            client_id = %client_id,
                            errno = data.errno,
                            alias = %alias,
                            command = %command,
                            "failed exec logged"
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to log failed exec: {}", e);
                        let item = RetryItem::LogFailed {
                            client_id,
                            command: command.clone(),
                            alias: alias.clone(),
                            process_id: data.pid,
                            timestamp,
                            errno: data.errno,
                        };
                        if let Err(e2) = retry_buffer.append(&item) {
                            tracing::error!("Failed to buffer failed exec: {}", e2);
                        }
                    }
                }
            }
            EbpfEvent::Exit(data) => {
                if let Some(meta) = tracker.remove(data.pid) {
                    match exporter.end_session(meta.session_id, &meta.client_id).await {
                        Ok(_) => {
                            let duration = (data.exit_ns / 1_000_000_000) as i64 - meta.start_time;
                            tracing::debug!(
                                pid = data.pid,
                                session_id = meta.session_id,
                                duration = duration,
                                "session ended"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                session_id = meta.session_id,
                                "Failed to end session: {}",
                                e
                            );
                            let item = RetryItem::EndSession {
                                session_id: meta.session_id,
                                client_id: meta.client_id.clone(),
                            };
                            if let Err(e2) = retry_buffer.append(&item) {
                                tracing::error!("Failed to buffer end session: {}", e2);
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = shutdown_tx.send(true);
    poll_handle.await.ok();

    // Drain remaining tracker entries (orphans)
    let orphans = tracker.drain();
    if !orphans.is_empty() {
        tracing::warn!("{} orphan sessions on shutdown", orphans.len());
        for (pid, meta) in orphans {
            tracing::warn!(pid = pid, session_id = meta.session_id, "orphan session");
        }
    }

    tracing::info!("tspan-ebpf daemon exited");
    Ok(())
}
