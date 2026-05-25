use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::exporter::Exporter;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum RetryItem {
    #[serde(rename = "start")]
    StartSession { command: String, process_id: u32, timestamp: i64 },
    #[serde(rename = "end")]
    EndSession { session_id: i64 },
    #[serde(rename = "failed")]
    LogFailed { command: String, process_id: u32, timestamp: i64, errno: i64 },
}

pub struct RetryBuffer {
    path: String,
}

impl RetryBuffer {
    pub fn new(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self { path: path.to_string() })
    }

    pub fn append(&self, item: &RetryItem) -> Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        let line = serde_json::to_string(item)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Replay buffered items. Returns the number of successfully replayed items.
    pub async fn replay(&self, exporter: &Exporter) -> Result<usize> {
        if !Path::new(&self.path).exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(0);
        }

        let mut replayed = 0;
        let mut remaining = Vec::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let item: RetryItem = match serde_json::from_str(line) {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!("Failed to parse retry line: {}", e);
                    remaining.push(line.to_string());
                    continue;
                }
            };

            let success = match &item {
                RetryItem::StartSession { command, process_id, timestamp } => {
                    exporter.start_session(command, *process_id, *timestamp).await.is_ok()
                }
                RetryItem::EndSession { session_id } => {
                    exporter.end_session(*session_id).await.is_ok()
                }
                RetryItem::LogFailed { command, process_id, timestamp, errno } => {
                    exporter.log_failed(command, *process_id, *timestamp, *errno).await.is_ok()
                }
            };

            if success {
                replayed += 1;
            } else {
                remaining.push(line.to_string());
            }
        }

        if remaining.is_empty() {
            let _ = fs::remove_file(&self.path);
        } else {
            let mut file = OpenOptions::new().create(true).truncate(true).write(true).open(&self.path)?;
            for line in remaining {
                writeln!(file, "{}", line)?;
            }
        }

        Ok(replayed)
    }
}
