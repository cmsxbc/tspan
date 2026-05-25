use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Exporter {
    client: reqwest::Client,
    server_url: String,
    token: String,
}

#[derive(Serialize)]
struct StartSessionReq {
    client_id: String,
    command: String,
    process_id: i64,
}

#[derive(Deserialize)]
struct StartSessionResp {
    session_id: i64,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct EndSessionResp {
    session_id: i64,
    duration_seconds: i64,
}

#[derive(Serialize)]
struct CreateExecEventReq {
    client_id: String,
    command: String,
    process_id: i64,
    timestamp: i64,
    errno: i64,
}

#[derive(Deserialize)]
struct CreateExecEventResp {
    record_id: i64,
}

impl Exporter {
    pub fn new(server_url: String, token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            server_url,
            token,
        }
    }

    pub async fn start_session(
        &self,
        client_id: &str,
        command: &str,
        process_id: u32,
        _timestamp: i64,
    ) -> Result<i64> {
        let url = format!("{}/api/sessions/start", self.server_url);
        let req = StartSessionReq {
            client_id: client_id.to_string(),
            command: command.to_string(),
            process_id: process_id as i64,
        };
        let resp: StartSessionResp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.session_id)
    }

    pub async fn end_session(&self, session_id: i64) -> Result<()> {
        let url = format!("{}/api/sessions/{}/end", self.server_url, session_id);
        self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn log_failed(
        &self,
        client_id: &str,
        command: &str,
        process_id: u32,
        timestamp: i64,
        errno: i64,
    ) -> Result<i64> {
        let url = format!("{}/api/exec-events", self.server_url);
        let req = CreateExecEventReq {
            client_id: client_id.to_string(),
            command: command.to_string(),
            process_id: process_id as i64,
            timestamp,
            errno,
        };
        let resp: CreateExecEventResp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.record_id)
    }
}
