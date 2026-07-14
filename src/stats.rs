use chrono::{DateTime, Datelike, LocalResult, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
pub struct PastNStat {
    pub name: String,
    pub seconds: i64,
    pub ratio: f64,
    pub times: i64,
    pub day_ratio: f64,
    pub days: i64,
    pub mean_usage: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IntervalStats {
    pub current_interval: i64,
    pub current_interval_hr: String,
    pub max_interval: i64,
    pub max_interval_hr: String,
    pub mean_interval: i64,
    pub mean_interval_hr: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Stats {
    pub total: TotalStats,
    pub past_n: Vec<PastNStat>,
    pub interval: IntervalStats,
}

pub fn resolve_tz(tz_name: Option<&str>) -> Tz {
    tz_name
        .and_then(|name| name.parse().ok())
        .unwrap_or(Tz::UTC)
}

/// Convert a Unix timestamp to a UTC datetime without panicking. Out-of-range
/// timestamps (e.g. a corrupt record) fall back to the epoch rather than
/// crashing the request thread.
fn ts_to_utc(ts: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(ts, 0).unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap())
}

/// Timestamp (seconds since epoch) for the start of `date` in `tz`, handling
/// DST transitions without panicking. A nonexistent local midnight
/// (spring-forward gap, e.g. America/Santiago) advances to the first valid
/// wall-clock time that day; an ambiguous local midnight (fall-back) uses the
/// earlier instant.
fn local_date_start(date: NaiveDate, tz: &Tz) -> i64 {
    for minute in 0..(24 * 60) {
        let naive = date.and_hms_opt(minute / 60, minute % 60, 0).unwrap();
        match tz.from_local_datetime(&naive) {
            LocalResult::Single(dt) => return dt.timestamp(),
            LocalResult::Ambiguous(earliest, _latest) => return earliest.timestamp(),
            LocalResult::None => continue,
        }
    }
    // No valid wall-clock time all day is impossible for real zones; fall back
    // to interpreting midnight as UTC rather than panicking.
    date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()
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

pub fn compute_stats(conn: &mut Connection, client_id: &str, alias: &str, command: &str, tz: &Tz) -> anyhow::Result<Stats> {
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
        "SELECT start_time, duration_seconds FROM records WHERE {} ORDER BY start_time ASC",
        wc
    ))?;
    let rows: Vec<(i64, i64)> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?.collect::<Result<Vec<_>, _>>()?;

    let now_utc = Utc::now();
    let now_local = now_utc.with_timezone(tz);
    let today_local = now_local.date_naive();
    if rows.is_empty() {
        let total = TotalStats {
            total_days: 0,
            active_days: 0,
            total_seconds: 0,
            total_times: 0,
            mean_usage: 0,
            total_ratio: 0.0,
            total_day_ratio: 0.0,
            from_date: today_local.format("%Y-%m-%d").to_string(),
            total_duration_hr: human_readable_time(0),
            total_seconds_hr: human_readable_time(0),
            mean_usage_hr: human_readable_time(0),
        };
        let interval = IntervalStats {
            current_interval: 0,
            current_interval_hr: human_readable_time(0),
            max_interval: 0,
            max_interval_hr: human_readable_time(0),
            mean_interval: 0,
            mean_interval_hr: human_readable_time(0),
        };
        return Ok(Stats { total, past_n: vec![], interval });
    }

    let earliest_utc = ts_to_utc(rows.first().unwrap().0);
    let earliest_local = earliest_utc.with_timezone(tz);
    let earliest_date = earliest_local.date_naive();

    let total_duration = (today_local - earliest_date).num_seconds();
    let total_days = total_duration / 86400;
    let total_seconds: i64 = rows.iter().map(|(_, d)| d).sum();
    let total_times = rows.len() as i64;
    let mean_usage = if total_times > 0 { total_seconds / total_times } else { 0 };

    let mut active_dates = HashSet::new();
    for (start_time, _) in &rows {
        let dt = ts_to_utc(*start_time).with_timezone(tz);
        active_dates.insert(dt.date_naive());
    }
    let active_days = active_dates.len() as i64;

    let total_ratio = calc_ratio(total_seconds, total_duration);
    let total_day_ratio = calc_ratio(active_days, total_days.max(1));

    let from_date = earliest_local.format("%Y-%m-%d").to_string();

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
        let cutoff_local = today_local - chrono::Duration::seconds(secs);
        let cutoff = local_date_start(cutoff_local, tz);

        let mut period_seconds = 0i64;
        let mut period_times = 0i64;
        let mut period_dates = HashSet::new();

        for (start_time, duration) in &rows {
            if *start_time > cutoff {
                period_seconds += duration;
                period_times += 1;
                let dt = ts_to_utc(*start_time).with_timezone(tz);
                period_dates.insert(dt.date_naive());
            }
        }

        let mean = if period_times > 0 { period_seconds / period_times } else { 0 };
        let ratio = calc_ratio(period_seconds, secs);
        let days = secs / 86400;
        let day_ratio = calc_ratio(period_dates.len() as i64, days.max(1));
        past_n.push(PastNStat {
            name: name.to_string(),
            seconds: period_seconds,
            ratio,
            times: period_times,
            day_ratio,
            days: period_dates.len() as i64,
            mean_usage: mean,
        });
    }

    let starts: Vec<i64> = rows.iter().map(|(s, _)| *s).collect();
    let current_interval = if let Some(last) = starts.last() {
        now_utc.timestamp() - last
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

#[derive(Debug, Deserialize, Serialize)]
pub struct ClientStat {
    pub client_id: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_seconds: i64,
    pub total_seconds_hr: String,
    pub mean_seconds_hr: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AliasStat {
    pub alias: String,
    pub total_seconds: i64,
    pub total_times: i64,
    pub mean_seconds: i64,
    pub total_seconds_hr: String,
    pub mean_seconds_hr: String,
}

#[derive(Debug, Deserialize, Serialize)]
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

pub fn get_daily_data(conn: &mut Connection, client_id: &str, alias: &str, command: &str, tz: &Tz) -> anyhow::Result<Vec<(String, i64)>> {
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
        "SELECT start_time, duration_seconds FROM records WHERE {} ORDER BY start_time ASC",
        wc
    ))?;
    let rows: Vec<(i64, i64)> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?.collect::<Result<Vec<_>, _>>()?;

    let mut day_map: HashMap<NaiveDate, i64> = HashMap::new();
    for (start_time, duration) in rows {
        let utc = ts_to_utc(start_time);
        let local = utc.with_timezone(tz);
        let day = local.date_naive();
        *day_map.entry(day).or_insert(0) += duration.max(0);
    }

    let mut result: Vec<(String, i64)> = day_map
        .into_iter()
        .map(|(day, seconds)| (day.format("%Y-%m-%d").to_string(), seconds))
        .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}
#[derive(Debug, Deserialize, Serialize)]
pub struct SessionBucket {
    pub label: String,
    pub count: i64,
    pub pct: f64,
}

#[derive(Debug, Deserialize, Serialize)]
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
    tz: &Tz,
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

    let mut stmt = conn.prepare(&format!(
        "SELECT start_time, duration_seconds FROM records WHERE {}",
        wc
    ))?;
    let rows: Vec<(i64, i64)> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?.collect::<Result<Vec<_>, _>>()?;

    let mut weekday_total = 0i64;
    let mut weekday_times = 0i64;
    let mut weekend_total = 0i64;
    let mut weekend_times = 0i64;
    for (start_time, duration) in rows {
        let utc = ts_to_utc(start_time);
        let local = utc.with_timezone(tz);
        let is_weekend = matches!(local.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun);
        if is_weekend {
            weekend_total += duration;
            weekend_times += 1;
        } else {
            weekday_total += duration;
            weekday_times += 1;
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

#[derive(Debug, Deserialize, Serialize)]
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
    tz: &Tz,
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
        "SELECT start_time FROM records WHERE {} ORDER BY start_time ASC",
        wc
    ))?;
    let starts: Vec<i64> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| row.get(0),
    )?.collect::<Result<Vec<_>, _>>()?;

    if starts.is_empty() {
        return Ok(StreakStats { current_streak: 0, max_streak: 0, last_active_date: "-".to_string(), last_active_time_hr: "-".to_string() });
    }

    let mut days: Vec<NaiveDate> = starts
        .into_iter()
        .map(|ts| ts_to_utc(ts).with_timezone(tz).date_naive())
        .collect();
    days.sort();
    days.dedup();

    let mut max_streak = 1i64;
    let mut current_streak = 1i64;

    for i in 1..days.len() {
        let diff = (days[i] - days[i - 1]).num_days();
        if diff == 1 {
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
        .map(|d| d.with_timezone(tz).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string());

    Ok(StreakStats {
        current_streak,
        max_streak,
        last_active_date: days.last().unwrap().format("%Y-%m-%d").to_string(),
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
    tz: &Tz,
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
        "SELECT start_time, duration_seconds FROM records WHERE {} ORDER BY start_time ASC",
        wc
    ))?;
    let rows: Vec<(i64, i64)> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?.collect::<Result<Vec<_>, _>>()?;

    let mut month_map: HashMap<String, (i64, i64)> = HashMap::new();
    for (start_time, duration) in rows {
        let utc = ts_to_utc(start_time);
        let local = utc.with_timezone(tz);
        let ym = local.format("%Y-%m").to_string();
        let entry = month_map.entry(ym).or_insert((0, 0));
        entry.0 += duration;
        entry.1 += 1;
    }

    let mut result: Vec<MonthlyPoint> = month_map
        .into_iter()
        .map(|(ym, (seconds, times))| MonthlyPoint {
            year_month: ym,
            total_seconds: seconds,
            total_times: times,
            total_seconds_hr: human_readable_time(seconds),
        })
        .collect();
    result.sort_by(|a, b| a.year_month.cmp(&b.year_month));
    Ok(result)
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
    tz: &Tz,
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
        "SELECT start_time, duration_seconds FROM records WHERE {}",
        wc
    ))?;
    let rows: Vec<(i64, i64)> = stmt.query_map(
        rusqlite::params_from_iter(&param_refs),
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?.collect::<Result<Vec<_>, _>>()?;

    let mut grid = vec![vec![0i64; 24]; 7];
    let mut max_seconds = 0i64;
    for (start_time, duration) in rows {
        let utc = ts_to_utc(start_time);
        let local = utc.with_timezone(tz);
        let dow = local.weekday().num_days_from_monday() as usize;
        let hour = local.hour() as usize;
        grid[dow][hour] += duration;
        if grid[dow][hour] > max_seconds {
            max_seconds = grid[dow][hour];
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
    fn local_date_start_survives_dst_gap() {
        // Chile springs forward at midnight: 2024-09-08 00:00 local does not exist.
        // This is the case that used to panic via from_local_datetime().unwrap().
        let tz: Tz = "America/Santiago".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 9, 8).unwrap();
        let ts = local_date_start(date, &tz);
        let resolved = ts_to_utc(ts).with_timezone(&tz);
        assert_eq!(resolved.date_naive(), date);
        // Midnight is skipped; the first valid wall-clock time is 01:00.
        assert_eq!(resolved.hour(), 1);
        assert_eq!(resolved.minute(), 0);
    }

    #[test]
    fn local_date_start_normal_day_is_midnight() {
        let tz: Tz = "America/New_York".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        let resolved = ts_to_utc(local_date_start(date, &tz)).with_timezone(&tz);
        assert_eq!(resolved.date_naive(), date);
        assert_eq!(resolved.hour(), 0);
        assert_eq!(resolved.minute(), 0);
    }

    #[test]
    fn ts_to_utc_handles_out_of_range_without_panicking() {
        // Far outside chrono's representable range; must fall back, not panic.
        assert_eq!(ts_to_utc(i64::MAX), DateTime::from_timestamp(0, 0).unwrap());
        assert_eq!(ts_to_utc(i64::MIN), DateTime::from_timestamp(0, 0).unwrap());
    }

    #[test]
    fn compute_stats_does_not_panic_in_dst_gap_timezone() {
        let mut conn = setup();
        insert_completed(&mut conn, "c1", "vim a", 60);
        insert_completed(&mut conn, "c1", "vim b", 120);
        let tz: Tz = "America/Santiago".parse().unwrap();
        // Exercises the past-N cutoff path (local_date_start) across many periods.
        let stats = compute_stats(&mut conn, "c1", "", "", &tz).unwrap();
        assert_eq!(stats.total.total_seconds, 180);
        assert!(!stats.past_n.is_empty());
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
