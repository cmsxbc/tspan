use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TotalStats {
    pub total_days: i64,
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

#[derive(Debug, Serialize)]
pub struct PastNStat {
    pub name: String,
    pub seconds: i64,
    pub ratio: f64,
    pub times: i64,
    pub day_ratio: f64,
    pub mean_usage: i64,
}

#[derive(Debug, Serialize)]
pub struct IntervalStats {
    pub current_interval: i64,
    pub max_interval: i64,
    pub mean_interval: i64,
    pub mean_interval_hr: String,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    pub total: TotalStats,
    pub past_n: Vec<PastNStat>,
    pub interval: IntervalStats,
}

pub fn human_readable_time(seconds: i64) -> String {
    if seconds <= 0 {
        return "0 seconds".to_string();
    }
    let mut parts = vec![];
    let mut rem = seconds;
    let days = rem / 86400;
    rem %= 86400;
    let hours = rem / 3600;
    rem %= 3600;
    let mins = rem / 60;
    rem %= 60;
    if days > 0 {
        parts.push(format!("{} days", days));
    }
    if hours > 0 {
        parts.push(format!("{} hours", hours));
    }
    if mins > 0 {
        parts.push(format!("{} minutes", mins));
    }
    if rem > 0 {
        parts.push(format!("{} seconds", rem));
    }
    parts.join(" ")
}

pub fn calc_ratio(value: i64, total: i64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (value as f64 * 100.0) / total as f64
}

fn client_filter(client_id: &str) -> &'static str {
    if client_id == "__global__" {
        ""
    } else {
        " AND client_id = ?1"
    }
}

pub fn compute_stats(conn: &mut Connection, client_id: &str) -> anyhow::Result<Stats> {
    let today = Utc::now().timestamp();
    let filter = client_filter(client_id);

    let (total_seconds, total_times, earliest_start): (i64, i64, Option<i64>) = if client_id == "__global__" {
        conn.query_row(
            &format!("SELECT COALESCE(SUM(duration_seconds), 0), COUNT(*), MIN(start_time) FROM records WHERE status = 'completed'{}", filter),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
    } else {
        conn.query_row(
            &format!("SELECT COALESCE(SUM(duration_seconds), 0), COUNT(*), MIN(start_time) FROM records WHERE status = 'completed'{}", filter),
            [client_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
    };

    let earliest_start = earliest_start.unwrap_or(today);
    let total_duration = today - earliest_start;
    let total_days = total_duration / 86400;
    let mean_usage = if total_times > 0 { total_seconds / total_times } else { 0 };
    let total_ratio = calc_ratio(total_seconds, total_duration);
    let total_day_ratio = calc_ratio(total_times, total_days.max(1));

    let from_date = DateTime::from_timestamp(earliest_start, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();

    let total = TotalStats {
        total_days,
        total_seconds,
        total_times,
        mean_usage,
        total_ratio,
        total_day_ratio,
        from_date,
        total_duration_hr: human_readable_time(total_duration),
        total_seconds_hr: human_readable_time(total_seconds),
        mean_usage_hr: human_readable_time(mean_usage),
    };

    let past_n_names = vec![
        ("1 week", 7 * 86400),
        ("2 weeks", 14 * 86400),
        ("1 month", 30 * 86400),
        ("3 months", 90 * 86400),
        ("6 months", 180 * 86400),
        ("1 year", 365 * 86400),
    ];

    let mut past_n = Vec::new();
    for (name, secs) in past_n_names {
        let start = today - secs;
        let (seconds, times): (i64, i64) = if client_id == "__global__" {
            conn.query_row(
                "SELECT COALESCE(SUM(duration_seconds), 0), COUNT(*) FROM records WHERE status = 'completed' AND start_time > ?1",
                params![start],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
        } else {
            conn.query_row(
                "SELECT COALESCE(SUM(duration_seconds), 0), COUNT(*) FROM records WHERE status = 'completed' AND client_id = ?1 AND start_time > ?2",
                params![client_id, start],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
        };
        let mean = if times > 0 { seconds / times } else { 0 };
        let ratio = calc_ratio(seconds, secs);
        let day_ratio = calc_ratio(times, (secs / 86400).max(1));
        past_n.push(PastNStat {
            name: name.to_string(),
            seconds,
            ratio,
            times,
            day_ratio,
            mean_usage: mean,
        });
    }

    let starts: Vec<i64> = {
        if client_id == "__global__" {
            let mut stmt = conn.prepare("SELECT start_time FROM records WHERE status = 'completed' ORDER BY start_time ASC")?;
            let rows = stmt.query_map([], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?;
            rows
        } else {
            let mut stmt = conn.prepare("SELECT start_time FROM records WHERE status = 'completed' AND client_id = ?1 ORDER BY start_time ASC")?;
            let rows = stmt.query_map([client_id], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?;
            rows
        }
    };

    let current_interval = if let Some(last) = starts.last() {
        (today - last) / 86400
    } else {
        0
    };

    let mut max_interval = 0i64;
    for i in 0..starts.len().saturating_sub(1) {
        let interval = (starts[i + 1] - starts[i]) / 86400;
        if interval > max_interval {
            max_interval = interval;
        }
    }
    let mean_interval = if total_times > 0 { total_duration / total_times } else { 0 };

    let interval = IntervalStats {
        current_interval,
        max_interval,
        mean_interval,
        mean_interval_hr: human_readable_time(mean_interval),
    };

    Ok(Stats { total, past_n, interval })
}

pub fn get_daily_data(conn: &mut Connection, client_id: &str) -> anyhow::Result<Vec<(String, i64)>> {
    if client_id == "__global__" {
        let mut stmt = conn.prepare(
            "SELECT date(start_time, 'unixepoch', 'localtime') as day,
                    COALESCE(SUM(duration_seconds), 0)
             FROM records WHERE status = 'completed'
             GROUP BY day
             ORDER BY day ASC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    } else {
        let mut stmt = conn.prepare(
            "SELECT date(start_time, 'unixepoch', 'localtime') as day,
                    COALESCE(SUM(duration_seconds), 0)
             FROM records WHERE status = 'completed' AND client_id = ?1
             GROUP BY day
             ORDER BY day ASC"
        )?;
        let rows = stmt.query_map([client_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
