#![allow(
    dead_code,
    reason = "response models intentionally mirror the complete server API payloads"
)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct TotalStats {
    pub total_days: i64,
    pub active_days: i64,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_usage: i64,
    pub total_ratio: f64,
    pub total_day_ratio: f64,
    pub from_date: String,
    pub total_duration_hr: String,
    pub total_seconds_hr: String,
    pub mean_usage_hr: String,
}

#[derive(Debug, Deserialize)]
pub struct PastNStat {
    pub name: String,
    pub seconds: i64,
    pub ratio: f64,
    pub times: i64,
    pub day_ratio: f64,
    pub days: i64,
    pub mean_usage: i64,
}

#[derive(Debug, Deserialize)]
pub struct IntervalStats {
    pub current_interval: i64,
    pub current_interval_hr: String,
    pub max_interval: i64,
    pub max_interval_hr: String,
    pub mean_interval: i64,
    pub mean_interval_hr: String,
}

#[derive(Debug, Deserialize)]
pub struct Stats {
    pub total: TotalStats,
    pub past_n: Vec<PastNStat>,
    pub interval: IntervalStats,
}

#[derive(Debug, Deserialize)]
pub struct ClientStat {
    pub client_id: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_seconds: i64,
    pub total_seconds_hr: String,
    pub mean_seconds_hr: String,
}

#[derive(Debug, Deserialize)]
pub struct AliasStat {
    pub alias: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_seconds: i64,
    pub total_seconds_hr: String,
    pub mean_seconds_hr: String,
}

#[derive(Debug, Deserialize)]
pub struct CommandStat {
    pub command: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_seconds: i64,
    pub total_seconds_hr: String,
    pub mean_seconds_hr: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionBucket {
    pub label: String,
    pub count: i64,
    pub pct: f64,
}

#[derive(Debug, Deserialize)]
pub struct SessionDistribution {
    pub max_seconds: i64,
    pub min_seconds: i64,
    pub median_seconds: i64,
    pub mean_seconds: i64,
    pub total_sessions: i64,
    pub max_seconds_hr: String,
    pub min_seconds_hr: String,
    pub median_seconds_hr: String,
    pub mean_seconds_hr: String,
    pub buckets: Vec<SessionBucket>,
}

#[derive(Debug, Deserialize)]
pub struct StreakStats {
    pub current_streak: i64,
    pub max_streak: i64,
    pub last_active_date: String,
    pub last_active_time_hr: String,
}

#[derive(Debug, Deserialize)]
pub struct EndSessionResp {
    pub session_id: i64,
    pub duration_seconds: i64,
}

#[derive(Debug, Deserialize)]
pub struct OrphanedSession {
    pub id: i64,
    pub client_id: String,
    pub start_time: i64,
    pub running_seconds: i64,
    pub command: Option<String>,
    pub alias: Option<String>,
    pub process_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CreateTokenReq {
    pub client_id: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTokenResp {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct RecordPageItem {
    pub id: i64,
    pub client_id: String,
    pub alias: Option<String>,
    pub command: Option<String>,
    pub start_time: i64,
    pub end_time: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct RecordsPageResp {
    pub records: Vec<RecordPageItem>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}

pub fn human_readable_time(seconds: i64) -> String {
    if seconds <= 0 {
        return "0 s".to_string();
    }
    let mut parts = vec![];
    let mut rem = seconds;
    let days = rem / 86_400;
    rem %= 86_400;
    let hours = rem / 3_600;
    rem %= 3_600;
    let minutes = rem / 60;
    rem %= 60;
    if days > 0 {
        parts.push(format!("{days} d"));
    }
    if hours > 0 || (days > 0 && (minutes > 0 || rem > 0)) {
        parts.push(format!("{hours:02} h"));
    }
    if minutes > 0 || (hours > 0 && rem > 0) || (days > 0 && rem > 0) {
        parts.push(format!("{minutes:02} m"));
    }
    parts.push(format!("{rem:02} s"));
    parts.join(" ")
}
