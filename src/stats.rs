use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
pub struct PastNStat {
    pub name: String,
    pub seconds: i64,
    pub ratio: f64,
    pub times: i64,
    pub day_ratio: f64,
    pub days: i64,
    pub mean_usage: i64,
}

#[derive(Debug, Serialize)]
pub struct IntervalStats {
    pub current_interval: i64,
    pub current_interval_hr: String,
    pub max_interval: i64,
    pub max_interval_hr: String,
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
        return "0 s".to_string();
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
        parts.push(format!("{} d", days));
    }
    if hours > 0 || (days > 0 && (mins > 0 || rem > 0)) {
        parts.push(format!("{:02} h", hours));
    }
    if mins > 0 || (hours > 0 && rem > 0) || (days > 0 && rem > 0) {
        parts.push(format!("{:02} m", mins));
    }
    parts.push(format!("{:02} s", rem));
    parts.join(" ")
}

pub fn calc_ratio(value: i64, total: i64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (value as f64 * 100.0) / total as f64
}

pub fn compute_stats(conn: &mut Connection, client_id: &str, alias: &str, command: &str) -> anyhow::Result<Stats> {
    let today = Utc::now().timestamp();

    let mut conditions = vec!["status = 'completed'".to_string()];
    let mut param_refs: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if client_id != "__global__" && !client_id.is_empty() {
        conditions.push("client_id = ?".to_string());
        param_refs.push(&client_id);
    }
    if !alias.is_empty() {
        conditions.push("alias = ?".to_string());
        param_refs.push(&alias);
    }
    let cmd_like;
    if !command.is_empty() {
        conditions.push("command LIKE ?".to_string());
        cmd_like = format!("%{}%", command);
        param_refs.push(&cmd_like);
    }
    let wc = conditions.join(" AND ");

    let (total_seconds, total_times, earliest_start, active_days): (i64, i64, Option<i64>, i64) = conn.query_row(
        &format!("SELECT COALESCE(SUM(duration_seconds), 0), COUNT(*), MIN(start_time), COALESCE(COUNT(DISTINCT strftime('%Y-%m-%d', start_time, 'unixepoch')), 0) FROM records WHERE {}", wc),
        rusqlite::params_from_iter(&param_refs),
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;

    let earliest_start = earliest_start.unwrap_or(today);
    let total_duration = today - earliest_start;
    let total_days = total_duration / 86400;
    let mean_usage = if total_times > 0 { total_seconds / total_times } else { 0 };
    let total_ratio = calc_ratio(total_seconds, total_duration);
    let total_day_ratio = calc_ratio(active_days, total_days.max(1));

    let from_date = DateTime::from_timestamp(earliest_start, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();

    let total = TotalStats {
        total_days,
        active_days,
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

    let mut past_n_names: Vec<(String, i64)> = vec![
        ("1 week".to_string(), 7 * 86400),
        ("2 weeks".to_string(), 14 * 86400),
        ("1 month".to_string(), 30 * 86400),
    ];
    if total_duration >= 90 * 86400 {
        past_n_names.push(("3 months".to_string(), 90 * 86400));
    }
    if total_duration >= 180 * 86400 {
        past_n_names.push(("6 months".to_string(), 180 * 86400));
    }
    if total_duration >= 365 * 86400 {
        past_n_names.push(("1 year".to_string(), 365 * 86400));
    }
    let year_secs = 365 * 86400;
    let mut multi_year = 2 * year_secs;
    while total_duration >= multi_year {
        let years = multi_year / year_secs;
        past_n_names.push((format!("{} years", years), multi_year));
        multi_year *= 2;
    }
    let all_time_label = if total_duration < 365 * 86400 {
        format!("All Time ({:.1} months)", total_duration as f64 / (30.0 * 86400.0))
    } else {
        format!("All Time ({:.1} years)", total_duration as f64 / (365.0 * 86400.0))
    };
    past_n_names.push((all_time_label, total_duration));

    let mut past_n = Vec::new();
    for (name, secs) in past_n_names {
        let start = today - secs;
        let mut c2 = conditions.clone();
        c2.push("start_time > ?".to_string());
        let mut p2 = param_refs.clone();
        p2.push(&start);
        let wc2 = c2.join(" AND ");
        let (seconds, times, active_days): (i64, i64, i64) = conn.query_row(
            &format!("SELECT COALESCE(SUM(duration_seconds), 0), COUNT(*), COALESCE(COUNT(DISTINCT strftime('%Y-%m-%d', start_time, 'unixepoch')), 0) FROM records WHERE {}", wc2),
            rusqlite::params_from_iter(&p2),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let mean = if times > 0 { seconds / times } else { 0 };
        let ratio = calc_ratio(seconds, secs);
        let days = secs / 86400;
        let day_ratio = calc_ratio(active_days, days.max(1));
        past_n.push(PastNStat {
            name: name.to_string(),
            seconds,
            ratio,
            times,
            day_ratio,
            days: active_days,
            mean_usage: mean,
        });
    }

    let wc_start = conditions.join(" AND ");
    let mut stmt = conn.prepare(&format!(
        "SELECT start_time FROM records WHERE {} ORDER BY start_time ASC",
        wc_start
    ))?;
    let starts: Vec<i64> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| row.get(0),
    )?.collect::<Result<Vec<_>, _>>()?;

    let current_interval = if let Some(last) = starts.last() {
        today - last
    } else {
        0
    };

    let mut max_interval = 0i64;
    for i in 0..starts.len().saturating_sub(1) {
        let interval = starts[i + 1] - starts[i];
        if interval > max_interval {
            max_interval = interval;
        }
    }
    let mean_interval = if total_times > 0 { total_duration / total_times } else { 0 };

    let interval = IntervalStats {
        current_interval,
        current_interval_hr: human_readable_time(current_interval),
        max_interval,
        max_interval_hr: human_readable_time(max_interval),
        mean_interval,
        mean_interval_hr: human_readable_time(mean_interval),
    };

    Ok(Stats { total, past_n, interval })
}

#[derive(Debug, Serialize)]
pub struct ClientStat {
    pub client_id: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_seconds: i64,
    pub total_seconds_hr: String,
    pub mean_seconds_hr: String,
}

#[derive(Debug, Serialize)]
pub struct AliasStat {
    pub alias: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_seconds: i64,
    pub total_seconds_hr: String,
    pub mean_seconds_hr: String,
}

#[derive(Debug, Serialize)]
pub struct CommandStat {
    pub command: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_seconds: i64,
    pub total_seconds_hr: String,
    pub mean_seconds_hr: String,
}

pub fn compute_stats_by_client(conn: &mut Connection, client_id: &str) -> anyhow::Result<Vec<ClientStat>> {
    let is_global = client_id == "__global__";
    let sql = if is_global {
        "SELECT client_id, COALESCE(SUM(duration_seconds), 0), COUNT(*)
         FROM records WHERE status = 'completed'
         GROUP BY client_id
         ORDER BY 2 DESC"
    } else {
        "SELECT client_id, COALESCE(SUM(duration_seconds), 0), COUNT(*)
         FROM records WHERE status = 'completed' AND client_id = ?1
         GROUP BY client_id
         ORDER BY 2 DESC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if is_global {
        stmt.query_map([], |row| {
            let total_seconds: i64 = row.get(1)?;
            let total_times: i64 = row.get(2)?;
            let mean = if total_times > 0 { total_seconds / total_times } else { 0 };
            Ok(ClientStat {
                client_id: row.get(0)?,
                total_seconds,
                total_times,
                mean_seconds: mean,
                total_seconds_hr: human_readable_time(total_seconds),
                mean_seconds_hr: human_readable_time(mean),
            })
        })?.collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([client_id], |row| {
            let total_seconds: i64 = row.get(1)?;
            let total_times: i64 = row.get(2)?;
            let mean = if total_times > 0 { total_seconds / total_times } else { 0 };
            Ok(ClientStat {
                client_id: row.get(0)?,
                total_seconds,
                total_times,
                mean_seconds: mean,
                total_seconds_hr: human_readable_time(total_seconds),
                mean_seconds_hr: human_readable_time(mean),
            })
        })?.collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

pub fn compute_stats_by_alias(conn: &mut Connection, client_id: &str) -> anyhow::Result<Vec<AliasStat>> {
    let is_global = client_id == "__global__";
    let sql = if is_global {
        "SELECT alias, COALESCE(SUM(duration_seconds), 0), COUNT(*)
         FROM records WHERE status = 'completed' AND alias IS NOT NULL AND alias != ''
         GROUP BY alias
         ORDER BY 2 DESC"
    } else {
        "SELECT alias, COALESCE(SUM(duration_seconds), 0), COUNT(*)
         FROM records WHERE status = 'completed' AND client_id = ?1 AND alias IS NOT NULL AND alias != ''
         GROUP BY alias
         ORDER BY 2 DESC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if is_global {
        stmt.query_map([], |row| {
            let total_seconds: i64 = row.get(1)?;
            let total_times: i64 = row.get(2)?;
            let mean = if total_times > 0 { total_seconds / total_times } else { 0 };
            Ok(AliasStat {
                alias: row.get(0)?,
                total_seconds,
                total_times,
                mean_seconds: mean,
                total_seconds_hr: human_readable_time(total_seconds),
                mean_seconds_hr: human_readable_time(mean),
            })
        })?.collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([client_id], |row| {
            let total_seconds: i64 = row.get(1)?;
            let total_times: i64 = row.get(2)?;
            let mean = if total_times > 0 { total_seconds / total_times } else { 0 };
            Ok(AliasStat {
                alias: row.get(0)?,
                total_seconds,
                total_times,
                mean_seconds: mean,
                total_seconds_hr: human_readable_time(total_seconds),
                mean_seconds_hr: human_readable_time(mean),
            })
        })?.collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

pub fn compute_stats_by_command(
    conn: &mut Connection,
    client_id: &str,
    depth: usize,
    token_limit: usize,
) -> anyhow::Result<Vec<CommandStat>> {
    let is_global = client_id == "__global__";
    let actual_depth = if depth == 0 { 0 } else { depth.min(token_limit) };

    let (group_expr, where_extra): (String, &str) = if actual_depth == 0 {
        ("command".to_string(), "AND command IS NOT NULL AND command != ''")
    } else {
        let mut expr = "json_extract(command_tokens, '$[0]')".to_string();
        for i in 1..actual_depth {
            expr.push_str(&format!(
                " || COALESCE(' ' || json_extract(command_tokens, '$[{}]'), '')",
                i
            ));
        }
        (expr, "AND command_tokens IS NOT NULL")
    };

    let client_filter = if is_global { "" } else { "AND client_id = ?1" };
    let sql = format!(
        "SELECT {}, COALESCE(SUM(duration_seconds), 0), COUNT(*)
         FROM records
         WHERE status = 'completed' {} {}
         GROUP BY {}
         ORDER BY 2 DESC",
        group_expr, client_filter, where_extra, group_expr
    );

    let mut stmt = conn.prepare(&sql)?;
    let row_mapper = |row: &rusqlite::Row| {
        let total_seconds: i64 = row.get(1)?;
        let total_times: i64 = row.get(2)?;
        let mean = if total_times > 0 { total_seconds / total_times } else { 0 };
        Ok(CommandStat {
            command: row.get(0)?,
            total_seconds,
            total_times,
            mean_seconds: mean,
            total_seconds_hr: human_readable_time(total_seconds),
            mean_seconds_hr: human_readable_time(mean),
        })
    };

    let rows = if is_global {
        stmt.query_map([], row_mapper)?.collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([client_id], row_mapper)?.collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

pub fn get_daily_data(conn: &mut Connection, client_id: &str, alias: &str, command: &str) -> anyhow::Result<Vec<(String, i64)>> {
    let mut conditions = vec!["status = 'completed'".to_string()];
    let mut param_refs: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if client_id != "__global__" && !client_id.is_empty() {
        conditions.push("client_id = ?".to_string());
        param_refs.push(&client_id);
    }
    if !alias.is_empty() {
        conditions.push("alias = ?".to_string());
        param_refs.push(&alias);
    }
    let cmd_like;
    if !command.is_empty() {
        conditions.push("command LIKE ?".to_string());
        cmd_like = format!("%{}%", command);
        param_refs.push(&cmd_like);
    }
    let wc = conditions.join(" AND ");

    let mut stmt = conn.prepare(&format!(
        "SELECT date(start_time, 'unixepoch', 'localtime') as day,
                COALESCE(SUM(duration_seconds), 0)
         FROM records WHERE {}
         GROUP BY day
         ORDER BY day ASC",
        wc
    ))?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
#[derive(Debug, Serialize)]
pub struct SessionBucket {
    pub label: String,
    pub count: i64,
    pub pct: f64,
}

#[derive(Debug, Serialize)]
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

pub fn compute_session_distribution(
    conn: &mut Connection,
    client_id: &str,
    alias: &str,
    command: &str,
) -> anyhow::Result<SessionDistribution> {
    let mut conditions = vec!["status = 'completed' AND duration_seconds IS NOT NULL".to_string()];
    let mut param_refs: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if client_id != "__global__" && !client_id.is_empty() {
        conditions.push("client_id = ?".to_string());
        param_refs.push(&client_id);
    }
    if !alias.is_empty() {
        conditions.push("alias = ?".to_string());
        param_refs.push(&alias);
    }
    let cmd_like;
    if !command.is_empty() {
        conditions.push("command LIKE ?".to_string());
        cmd_like = format!("%{}%", command);
        param_refs.push(&cmd_like);
    }
    let wc = conditions.join(" AND ");

    let mut stmt = conn.prepare(&format!(
        "SELECT duration_seconds FROM records WHERE {} ORDER BY duration_seconds ASC",
        wc
    ))?;
    let durations: Vec<i64> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| row.get(0),
    )?.collect::<Result<Vec<_>, _>>()?;

    let total = durations.len() as i64;
    if total == 0 {
        return Ok(SessionDistribution {
            max_seconds: 0, min_seconds: 0, median_seconds: 0, mean_seconds: 0, total_sessions: 0,
            max_seconds_hr: human_readable_time(0),
            min_seconds_hr: human_readable_time(0),
            median_seconds_hr: human_readable_time(0),
            mean_seconds_hr: human_readable_time(0),
            buckets: vec![],
        });
    }

    let max_s = *durations.last().unwrap();
    let min_s = *durations.first().unwrap();
    let sum: i64 = durations.iter().sum();
    let mean_s = sum / total;
    let median_s = if total % 2 == 1 {
        durations[((total - 1) / 2) as usize]
    } else {
        (durations[(total / 2 - 1) as usize] + durations[(total / 2) as usize]) / 2
    };

    let buckets_def = [
        (0, 300, "<5 min"),
        (300, 900, "5–15 min"),
        (900, 1800, "15–30 min"),
        (1800, 3600, "30–60 min"),
        (3600, 7200, "1–2 h"),
        (7200, 14400, "2–4 h"),
        (14400, i64::MAX, ">4 h"),
    ];
    let mut buckets = vec![];
    for (lo, hi, label) in &buckets_def {
        let cnt = durations.iter().filter(|&&d| d >= *lo && d < *hi).count() as i64;
        buckets.push(SessionBucket {
            label: label.to_string(),
            count: cnt,
            pct: if total > 0 { (cnt as f64 / total as f64) * 100.0 } else { 0.0 },
        });
    }

    Ok(SessionDistribution {
        max_seconds: max_s,
        min_seconds: min_s,
        median_seconds: median_s,
        mean_seconds: mean_s,
        total_sessions: total,
        max_seconds_hr: human_readable_time(max_s),
        min_seconds_hr: human_readable_time(min_s),
        median_seconds_hr: human_readable_time(median_s),
        mean_seconds_hr: human_readable_time(mean_s),
        buckets,
    })
}

#[derive(Debug, Serialize)]
pub struct WeekdayWeekendStats {
    pub weekday_total_seconds: i64,
    pub weekday_times: i64,
    pub weekday_mean_seconds: i64,
    pub weekend_total_seconds: i64,
    pub weekend_times: i64,
    pub weekend_mean_seconds: i64,
    pub weekday_total_hr: String,
    pub weekday_mean_hr: String,
    pub weekend_total_hr: String,
    pub weekend_mean_hr: String,
}

pub fn compute_weekday_weekend_stats(
    conn: &mut Connection,
    client_id: &str,
    alias: &str,
    command: &str,
) -> anyhow::Result<WeekdayWeekendStats> {
    let mut conditions = vec!["status = 'completed'".to_string()];
    let mut param_refs: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if client_id != "__global__" && !client_id.is_empty() {
        conditions.push("client_id = ?".to_string());
        param_refs.push(&client_id);
    }
    if !alias.is_empty() {
        conditions.push("alias = ?".to_string());
        param_refs.push(&alias);
    }
    let cmd_like;
    if !command.is_empty() {
        conditions.push("command LIKE ?".to_string());
        cmd_like = format!("%{}%", command);
        param_refs.push(&cmd_like);
    }
    let wc = conditions.join(" AND ");

    let sql = format!(
        "SELECT CASE WHEN strftime('%w', start_time, 'unixepoch', 'localtime') IN ('0','6') THEN 'weekend' ELSE 'weekday' END as day_type,
                COALESCE(SUM(duration_seconds), 0), COUNT(*)
         FROM records WHERE {}
         GROUP BY day_type",
        wc
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| {
            let day_type: String = row.get(0)?;
            let total_seconds: i64 = row.get(1)?;
            let total_times: i64 = row.get(2)?;
            let mean = if total_times > 0 { total_seconds / total_times } else { 0 };
            Ok((day_type, total_seconds, total_times, mean))
        },
    )?.collect::<Result<Vec<_>, _>>()?;

    let mut weekday_total = 0i64;
    let mut weekday_times = 0i64;
    let mut weekend_total = 0i64;
    let mut weekend_times = 0i64;
    for (dt, total, times, _) in rows {
        if dt == "weekday" {
            weekday_total = total;
            weekday_times = times;
        } else {
            weekend_total = total;
            weekend_times = times;
        }
    }

    Ok(WeekdayWeekendStats {
        weekday_total_seconds: weekday_total,
        weekday_times,
        weekday_mean_seconds: if weekday_times > 0 { weekday_total / weekday_times } else { 0 },
        weekend_total_seconds: weekend_total,
        weekend_times,
        weekend_mean_seconds: if weekend_times > 0 { weekend_total / weekend_times } else { 0 },
        weekday_total_hr: human_readable_time(weekday_total),
        weekday_mean_hr: human_readable_time(if weekday_times > 0 { weekday_total / weekday_times } else { 0 }),
        weekend_total_hr: human_readable_time(weekend_total),
        weekend_mean_hr: human_readable_time(if weekend_times > 0 { weekend_total / weekend_times } else { 0 }),
    })
}

#[derive(Debug, Serialize)]
pub struct StreakStats {
    pub current_streak: i64,
    pub max_streak: i64,
    pub last_active_date: String,
    pub last_active_time_hr: String,
}

pub fn compute_streaks(
    conn: &mut Connection,
    client_id: &str,
    alias: &str,
    command: &str,
) -> anyhow::Result<StreakStats> {
    let mut conditions = vec!["status = 'completed' AND duration_seconds > 0".to_string()];
    let mut param_refs: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if client_id != "__global__" && !client_id.is_empty() {
        conditions.push("client_id = ?".to_string());
        param_refs.push(&client_id);
    }
    if !alias.is_empty() {
        conditions.push("alias = ?".to_string());
        param_refs.push(&alias);
    }
    let cmd_like;
    if !command.is_empty() {
        conditions.push("command LIKE ?".to_string());
        cmd_like = format!("%{}%", command);
        param_refs.push(&cmd_like);
    }
    let wc = conditions.join(" AND ");

    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT date(start_time, 'unixepoch', 'localtime') as day FROM records WHERE {} ORDER BY day ASC",
        wc
    ))?;
    let days: Vec<String> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| row.get(0),
    )?.collect::<Result<Vec<_>, _>>()?;

    if days.is_empty() {
        return Ok(StreakStats { current_streak: 0, max_streak: 0, last_active_date: "-".to_string(), last_active_time_hr: "-".to_string() });
    }

    let mut max_streak = 1i64;
    let mut current_streak = 1i64;

    for i in 1..days.len() {
        let prev = chrono::NaiveDate::parse_from_str(&days[i - 1], "%Y-%m-%d").unwrap();
        let curr = chrono::NaiveDate::parse_from_str(&days[i], "%Y-%m-%d").unwrap();
        if (curr - prev).num_days() == 1 {
            current_streak += 1;
        } else {
            if current_streak > max_streak {
                max_streak = current_streak;
            }
            current_streak = 1;
        }
    }
    if current_streak > max_streak {
        max_streak = current_streak;
    }

    let last_active_time: Option<i64> = conn.query_row(
        &format!("SELECT MAX(start_time) FROM records WHERE {}", wc),
        rusqlite::params_from_iter(&param_refs),
        |row| row.get(0),
    )?;
    let last_active_time_hr = last_active_time
        .and_then(|ts| DateTime::from_timestamp(ts, 0))
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string());

    Ok(StreakStats {
        current_streak,
        max_streak,
        last_active_date: days.last().unwrap().clone(),
        last_active_time_hr,
    })
}

#[derive(Debug, Serialize)]
pub struct MonthlyPoint {
    pub year_month: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub total_seconds_hr: String,
}

pub fn compute_monthly_trend(
    conn: &mut Connection,
    client_id: &str,
    alias: &str,
    command: &str,
) -> anyhow::Result<Vec<MonthlyPoint>> {
    let mut conditions = vec!["status = 'completed'".to_string()];
    let mut param_refs: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if client_id != "__global__" && !client_id.is_empty() {
        conditions.push("client_id = ?".to_string());
        param_refs.push(&client_id);
    }
    if !alias.is_empty() {
        conditions.push("alias = ?".to_string());
        param_refs.push(&alias);
    }
    let cmd_like;
    if !command.is_empty() {
        conditions.push("command LIKE ?".to_string());
        cmd_like = format!("%{}%", command);
        param_refs.push(&cmd_like);
    }
    let wc = conditions.join(" AND ");

    let mut stmt = conn.prepare(&format!(
        "SELECT strftime('%Y-%m', start_time, 'unixepoch', 'localtime') as ym,
                COALESCE(SUM(duration_seconds), 0), COUNT(*)
         FROM records WHERE {}
         GROUP BY ym
         ORDER BY ym ASC",
        wc
    ))?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| {
            let total_seconds: i64 = row.get(1)?;
            Ok(MonthlyPoint {
                year_month: row.get(0)?,
                total_seconds,
                total_times: row.get(2)?,
                total_seconds_hr: human_readable_time(total_seconds),
            })
        },
    )?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug, Serialize)]
pub struct HourlyHeatmap {
    pub grid: Vec<Vec<i64>>,
    pub max_seconds: i64,
}

pub fn compute_hourly_heatmap(
    conn: &mut Connection,
    client_id: &str,
    alias: &str,
    command: &str,
) -> anyhow::Result<HourlyHeatmap> {
    let mut conditions = vec!["status = 'completed'".to_string()];
    let mut param_refs: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if client_id != "__global__" && !client_id.is_empty() {
        conditions.push("client_id = ?".to_string());
        param_refs.push(&client_id);
    }
    if !alias.is_empty() {
        conditions.push("alias = ?".to_string());
        param_refs.push(&alias);
    }
    let cmd_like;
    if !command.is_empty() {
        conditions.push("command LIKE ?".to_string());
        cmd_like = format!("%{}%", command);
        param_refs.push(&cmd_like);
    }
    let wc = conditions.join(" AND ");

    let mut stmt = conn.prepare(&format!(
        "SELECT CAST(strftime('%w', start_time, 'unixepoch', 'localtime') AS INTEGER) as dow,
                CAST(strftime('%H', start_time, 'unixepoch', 'localtime') AS INTEGER) as hour,
                COALESCE(SUM(duration_seconds), 0)
         FROM records WHERE {}
         GROUP BY dow, hour",
        wc
    ))?;

    let mut grid = vec![vec![0i64; 24]; 7];
    let rows = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| {
            let sqlite_dow: i32 = row.get(0)?;
            let hour: i32 = row.get(1)?;
            let seconds: i64 = row.get(2)?;
            let mapped_dow = if sqlite_dow == 0 { 6 } else { sqlite_dow - 1 };
            Ok((mapped_dow, hour, seconds))
        },
    )?.collect::<Result<Vec<_>, _>>()?;

    let mut max_seconds = 0i64;
    for (dow, hour, seconds) in rows {
        grid[dow as usize][hour as usize] = seconds;
        if seconds > max_seconds {
            max_seconds = seconds;
        }
    }

    Ok(HourlyHeatmap { grid, max_seconds })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_db, start_session};
    use rusqlite::{Connection, params};

    fn setup() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&mut conn).unwrap();
        conn
    }

    fn insert_completed(conn: &mut Connection, client_id: &str, command: &str, duration: i64) {
        let id = start_session(conn, client_id, Some(command), None, None).unwrap();
        conn.execute(
            "UPDATE records SET end_time = start_time + ?1, duration_seconds = ?1, status = 'completed' WHERE id = ?2",
            params![duration, id],
        ).unwrap();
    }

    #[test]
    fn test_by_command_depth_0_full_command() {
        let mut conn = setup();
        insert_completed(&mut conn, "c1", "perf stats record", 100);
        insert_completed(&mut conn, "c1", "perf stats report", 200);
        insert_completed(&mut conn, "c1", "python train.py", 300);

        let stats = compute_stats_by_command(&mut conn, "c1", 0, 5).unwrap();
        assert_eq!(stats.len(), 3);
        // Ordered by total_seconds DESC
        assert_eq!(stats[0].command, "python train.py");
        assert_eq!(stats[0].total_seconds, 300);
        assert_eq!(stats[1].command, "perf stats report");
        assert_eq!(stats[1].total_seconds, 200);
        assert_eq!(stats[2].command, "perf stats record");
        assert_eq!(stats[2].total_seconds, 100);
    }

    #[test]
    fn test_by_command_depth_1_base() {
        let mut conn = setup();
        insert_completed(&mut conn, "c1", "perf stats record", 100);
        insert_completed(&mut conn, "c1", "perf stats report", 200);
        insert_completed(&mut conn, "c1", "python train.py", 300);

        let stats = compute_stats_by_command(&mut conn, "c1", 1, 5).unwrap();
        assert_eq!(stats.len(), 2);
        // python = 300, perf = 300 (100+200)
        let python = stats.iter().find(|s| s.command == "python").unwrap();
        let perf = stats.iter().find(|s| s.command == "perf").unwrap();
        assert_eq!(python.total_seconds, 300);
        assert_eq!(perf.total_seconds, 300);
    }

    #[test]
    fn test_by_command_depth_2_sub() {
        let mut conn = setup();
        insert_completed(&mut conn, "c1", "perf stats record", 100);
        insert_completed(&mut conn, "c1", "perf stats report", 200);
        insert_completed(&mut conn, "c1", "python train.py", 300);

        let stats = compute_stats_by_command(&mut conn, "c1", 2, 5).unwrap();
        assert_eq!(stats.len(), 2);
        let perf_stats = stats.iter().find(|s| s.command == "perf stats").unwrap();
        let python_train = stats.iter().find(|s| s.command == "python train.py").unwrap();
        assert_eq!(perf_stats.total_seconds, 300);
        assert_eq!(python_train.total_seconds, 300);
    }

    #[test]
    fn test_by_command_depth_exceeds_token_limit() {
        let mut conn = setup();
        insert_completed(&mut conn, "c1", "perf stats record", 100);
        insert_completed(&mut conn, "c1", "perf stats report", 200);

        // limit=1 means depth=2 should behave like depth=1
        let stats = compute_stats_by_command(&mut conn, "c1", 2, 1).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].command, "perf");
        assert_eq!(stats[0].total_seconds, 300);
    }

    #[test]
    fn test_by_command_global_aggregation() {
        let mut conn = setup();
        insert_completed(&mut conn, "c1", "perf stats", 100);
        insert_completed(&mut conn, "c2", "perf stats", 200);

        let stats = compute_stats_by_command(&mut conn, "__global__", 1, 5).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].command, "perf");
        assert_eq!(stats[0].total_seconds, 300);
    }

    #[test]
    fn test_by_command_quoted_args() {
        let mut conn = setup();
        insert_completed(&mut conn, "c1", "ls \"my documents\"", 100);
        insert_completed(&mut conn, "c1", "ls downloads", 200);

        let stats = compute_stats_by_command(&mut conn, "c1", 2, 5).unwrap();
        assert_eq!(stats.len(), 2);
        let docs = stats.iter().find(|s| s.command == "ls my documents").unwrap();
        let downloads = stats.iter().find(|s| s.command == "ls downloads").unwrap();
        assert_eq!(docs.total_seconds, 100);
        assert_eq!(downloads.total_seconds, 200);
    }

    #[test]
    fn test_by_command_single_token() {
        let mut conn = setup();
        insert_completed(&mut conn, "c1", "vim", 100);
        insert_completed(&mut conn, "c1", "vim", 200);

        let stats_d0 = compute_stats_by_command(&mut conn, "c1", 0, 5).unwrap();
        assert_eq!(stats_d0.len(), 1);
        assert_eq!(stats_d0[0].command, "vim");

        let stats_d1 = compute_stats_by_command(&mut conn, "c1", 1, 5).unwrap();
        assert_eq!(stats_d1.len(), 1);
        assert_eq!(stats_d1[0].command, "vim");

        let stats_d2 = compute_stats_by_command(&mut conn, "c1", 2, 5).unwrap();
        assert_eq!(stats_d2.len(), 1);
        assert_eq!(stats_d2[0].command, "vim");
    }
}
