use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::auth::{AuthConfig, check_api_auth, check_web_auth};
use crate::db::{self, DbPool};
use crate::stats::{self, compute_stats};
use crate::svg_calendar::{generate_all_years_svgs, generate_svg_calendar};
use crate::markdown::generate_markdown_report;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub auth: AuthConfig,
}

#[derive(Deserialize)]
pub struct StartSessionReq {
    pub client_id: String,
    pub command: Option<String>,
    pub alias: Option<String>,
    pub process_id: Option<i64>,
}

#[derive(Serialize)]
pub struct StartSessionResp {
    pub session_id: i64,
    pub start_time: i64,
}

#[derive(Serialize)]
pub struct EndSessionResp {
    pub session_id: i64,
    pub duration_seconds: i64,
}

#[derive(Serialize)]
pub struct OrphanedSession {
    pub id: i64,
    pub client_id: String,
    pub start_time: i64,
    pub running_seconds: i64,
    pub command: Option<String>,
    pub alias: Option<String>,
    pub process_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct ImportReq {
    pub path: String,
}

pub fn create_router(state: AppState) -> Router {
    let api_routes = Router::new()
        .route("/sessions/start", post(api_start_session))
        .route("/sessions/:id/end", post(api_end_session))
        .route("/sessions/:id/discard", post(api_discard_session))
        .route("/sessions/orphaned", get(api_get_orphaned))
        .route("/stats", get(api_get_stats))
        .route("/stats/summary.md", get(api_get_summary_md))
        .route("/admin/import", post(api_import))
        .route("/admin/backup", get(api_backup))
        .route("/admin/tokens", get(api_list_tokens).post(api_create_token))
        .route("/admin/tokens/:token", post(api_revoke_token));

    let web_routes = Router::new()
        .route("/", get(web_index))
        .route("/admin", get(web_admin));

    Router::new()
        .nest("/api", api_routes)
        .merge(web_routes)
        .with_state(state)
}

async fn api_start_session(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(req): Json<StartSessionReq>,
) -> Result<Json<StartSessionResp>, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let id = db::start_session(
        &mut conn,
        &req.client_id,
        req.command.as_deref(),
        req.alias.as_deref(),
        req.process_id,
    ).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let now = chrono::Utc::now().timestamp();
    Ok(Json(StartSessionResp {
        session_id: id,
        start_time: now,
    }))
}

async fn api_end_session(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<EndSessionResp>, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let duration = db::end_session(&mut conn, id).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    match duration {
        Some(d) => Ok(Json(EndSessionResp { session_id: id, duration_seconds: d })),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn api_discard_session(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let ok = db::discard_session(&mut conn, id).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn api_get_orphaned(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<OrphanedSession>>, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let records = db::get_orphaned_sessions(&mut conn).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let now = chrono::Utc::now().timestamp();
    let result: Vec<OrphanedSession> = records.into_iter().map(|r| OrphanedSession {
        id: r.id,
        client_id: r.client_id,
        start_time: r.start_time,
        running_seconds: now - r.start_time,
        command: r.command,
        alias: r.alias,
        process_id: r.process_id,
    }).collect();

    Ok(Json(result))
}

async fn api_get_stats(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<stats::Stats>, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let stats = compute_stats(&mut conn).map_err(|e| {
        tracing::error!("Stats error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(stats))
}

async fn api_get_summary_md(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let stats = compute_stats(&mut conn).map_err(|e| {
        tracing::error!("Stats error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let daily = stats::get_daily_data(&mut conn).map_err(|e| {
        tracing::error!("Daily data error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let all_svg = generate_svg_calendar(&daily, None);
    let year_svgs = generate_all_years_svgs(&daily);

    let md = generate_markdown_report(&stats, &all_svg, &year_svgs);

    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        md,
    ).into_response())
}

async fn api_import(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(req): Json<ImportReq>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let result = crate::importer::import_from_directory(&state.pool, &req.path).await.map_err(|e| {
        tracing::error!("Import error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(serde_json::json!({
        "imported": result.imported,
        "failed": result.failed,
        "errors": result.errors,
    })))
}

async fn api_backup(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let temp_path = "/tmp/wyd_backup_tmp.db";
    {
        let conn = state.pool.lock().unwrap();
        let mut dest = rusqlite::Connection::open(temp_path).map_err(|e| {
            tracing::error!("Backup temp db error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        let backup = rusqlite::backup::Backup::new(&*conn, &mut dest).map_err(|e| {
            tracing::error!("Backup init error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        backup.run_to_completion(5, std::time::Duration::from_millis(100), None).map_err(|e| {
            tracing::error!("Backup run error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    let data = tokio::fs::read(temp_path).await.map_err(|e| {
        tracing::error!("Read backup file error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let _ = tokio::fs::remove_file(temp_path).await;

    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"wyd-backup.db\""),
        ],
        data,
    ).into_response())
}

async fn web_index(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    check_web_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let stats = compute_stats(&mut conn).map_err(|e| {
        tracing::error!("Stats error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let daily = stats::get_daily_data(&mut conn).map_err(|e| {
        tracing::error!("Daily data error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let all_svg = generate_svg_calendar(&daily, None);
    let year_svgs = generate_all_years_svgs(&daily);

    let mut html = String::new();
    html.push_str(r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>WYD Stats</title>
<style>
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;max-width:1200px;margin:0 auto;padding:20px;color:#333;background:#f6f8fa}
.card{background:#fff;border-radius:8px;padding:20px;margin-bottom:20px;box-shadow:0 1px 3px rgba(0,0,0,0.1)}
h1,h2{margin-top:0}
.stats-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:15px;margin-bottom:20px}
.stat-card{background:#fff;border-radius:8px;padding:15px;text-align:center;box-shadow:0 1px 3px rgba(0,0,0,0.1)}
.stat-value{font-size:24px;font-weight:bold;color:#0969da}
.stat-label{font-size:12px;color:#666;margin-top:5px}
table{width:100%;border-collapse:collapse;margin-top:10px}
th,td{padding:8px 12px;text-align:left;border-bottom:1px solid #eee}
th{font-size:12px;color:#666;font-weight:600}
.nav{margin-bottom:20px}
.nav a{color:#0969da;text-decoration:none;margin-right:15px}
</style></head><body>"#);

    html.push_str(r#"<div class="nav"><a href="/">Home</a><a href="/admin">Admin</a></div>"#);
    html.push_str("<h1>Activity Stats</h1>");

    html.push_str(r#"<div class="stats-grid">"#);
    html.push_str(&format!(r#"<div class="stat-card"><div class="stat-value">{}</div><div class="stat-label">Total Days</div></div>"#, stats.total.total_days));
    html.push_str(&format!(r#"<div class="stat-card"><div class="stat-value">{}</div><div class="stat-label">Total Times</div></div>"#, stats.total.total_times));
    html.push_str(&format!(r#"<div class="stat-card"><div class="stat-value">{}</div><div class="stat-label">Total Duration</div></div>"#, stats.total.total_seconds_hr));
    html.push_str(&format!(r#"<div class="stat-card"><div class="stat-value">{}</div><div class="stat-label">Mean / Session</div></div>"#, stats.total.mean_usage_hr));
    html.push_str("</div>");

    html.push_str(r#"<div class="card"><h2>Activity Graph (All Time)</h2>"#);
    html.push_str(&all_svg);
    html.push_str("</div>");

    for (year, svg) in year_svgs {
        html.push_str(&format!(r#"<div class="card"><h2>{}</h2>"#, year));
        html.push_str(&svg);
        html.push_str("</div>");
    }

    html.push_str(r#"<div class="card"><h2>Past N Stats</h2><table><tr><th>Period</th><th>Seconds</th><th>Ratio</th><th>Times</th><th>Day Ratio</th><th>Mean</th></tr>"#);
    for p in &stats.past_n {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{:.2}%</td><td>{}</td><td>{:.2}%</td><td>{}s</td></tr>",
            p.name, p.seconds, p.ratio, p.times, p.day_ratio, p.mean_usage
        ));
    }
    html.push_str("</table></div>");

    html.push_str(&format!(
        r#"<div class="card"><h2>Interval</h2><p>Current interval: {} days</p><p>Max interval: {} days</p><p>Mean interval: {}</p></div>"#,
        stats.interval.current_interval,
        stats.interval.max_interval,
        stats.interval.mean_interval_hr
    ));

    html.push_str("</body></html>");
    Ok(Html(html))
}

async fn web_admin(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    check_web_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let orphaned = db::get_orphaned_sessions(&mut conn).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let tokens = db::list_api_tokens(&mut conn).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let now = chrono::Utc::now().timestamp();

    let mut html = String::new();
    html.push_str(r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>WYD Admin</title>
<style>
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;max-width:1200px;margin:0 auto;padding:20px;color:#333;background:#f6f8fa}
.card{background:#fff;border-radius:8px;padding:20px;margin-bottom:20px;box-shadow:0 1px 3px rgba(0,0,0,0.1)}
h1,h2{margin-top:0}
table{width:100%;border-collapse:collapse;margin-top:10px}
th,td{padding:8px 12px;text-align:left;border-bottom:1px solid #eee}
th{font-size:12px;color:#666;font-weight:600}
.btn{padding:6px 12px;border:none;border-radius:4px;cursor:pointer;font-size:12px;margin-right:5px}
.btn-end{background:#2da44e;color:#fff}
.btn-discard{background:#cf222e;color:#fff}
.btn-gen{background:#0969da;color:#fff}
.nav{margin-bottom:20px}
.nav a{color:#0969da;text-decoration:none;margin-right:15px}
.code{font-family:monospace;background:#f6f8fa;padding:2px 6px;border-radius:3px;font-size:12px}
.token-preview{color:#666}
</style></head><body>"#);

    html.push_str(r#"<div class="nav"><a href="/">Home</a><a href="/admin">Admin</a></div>"#);
    html.push_str("<h1>Admin</h1>");

    // Orphaned sessions
    html.push_str(r#"<div class="card"><h2>Orphaned Sessions</h2>"#);
    if orphaned.is_empty() {
        html.push_str("<p>No orphaned sessions.</p>");
    } else {
        html.push_str(r#"<table><tr><th>ID</th><th>Client</th><th>Alias</th><th>Command</th><th>PID</th><th>Start</th><th>Running</th><th>Action</th></tr>"#);
        for r in &orphaned {
            let running = now - r.start_time;
            let start_str = chrono::DateTime::from_timestamp(r.start_time, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            let cmd = r.command.as_deref().unwrap_or("-");
            let alias = r.alias.as_deref().unwrap_or("-");
            let pid_str = r.process_id.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string());
            html.push_str(&format!(
                r#"<tr><td>{}</td><td>{}</td><td>{}</td><td class="code">{}</td><td>{}</td><td>{}</td><td>{}s</td>
                <td><button class="btn btn-end" onclick="endSession({})">End</button>
                <button class="btn btn-discard" onclick="discardSession({})">Discard</button></td></tr>"#,
                r.id, r.client_id, alias, html_escape(cmd), pid_str, start_str, running, r.id, r.id
            ));
        }
        html.push_str("</table>");
    }
    html.push_str("</div>");

    // API Tokens
    html.push_str(r#"<div class="card"><h2>API Tokens</h2>"#);
    html.push_str(r#"<button class="btn btn-gen" onclick="genToken()">Generate New Token</button>"#);
    html.push_str(r#"<div id="token-result" style="margin-top:10px"></div>"#);
    if tokens.is_empty() {
        html.push_str("<p>No tokens.</p>");
    } else {
        html.push_str(r#"<table><tr><th>Token</th><th>Description</th><th>Created</th><th>Action</th></tr>"#);
        for t in &tokens {
            let created = chrono::DateTime::from_timestamp(t.created_at, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            let preview = if t.token.len() > 12 {
                format!("{}****{}", &t.token[..6], &t.token[t.token.len()-4..])
            } else {
                "****".to_string()
            };
            html.push_str(&format!(
                r#"<tr><td class="token-preview">{}</td><td>{}</td><td>{}</td>
                <td><button class="btn btn-discard" onclick="revokeToken('{}')">Revoke</button></td></tr>"#,
                preview,
                t.description.as_deref().unwrap_or("-"),
                created,
                html_escape(&t.token)
            ));
        }
        html.push_str("</table>");
    }
    html.push_str("</div>");

    html.push_str(r#"<script>
async function endSession(id) {
    if(!confirm('End session ' + id + '?')) return;
    const r = await fetch('/api/sessions/' + id + '/end', {method:'POST'});
    if(r.ok) location.reload(); else alert('Failed: ' + r.status);
}
async function discardSession(id) {
    if(!confirm('Discard session ' + id + '?')) return;
    const r = await fetch('/api/sessions/' + id + '/discard', {method:'POST'});
    if(r.ok) location.reload(); else alert('Failed: ' + r.status);
}
async function genToken() {
    const desc = prompt('Token description:');
    if(!desc) return;
    const r = await fetch('/api/admin/tokens', {
        method:'POST',
        headers:{'Content-Type':'application/json'},
        body: JSON.stringify({description: desc})
    });
    if(r.ok) {
        const data = await r.json();
        document.getElementById('token-result').innerHTML =
            '<div style="background:#fff3cd;padding:10px;border-radius:4px;">' +
            '<strong>Copy this token now — it will not be shown again!</strong><br>' +
            '<code style="font-size:14px;background:#f6f8fa;padding:4px 8px;border-radius:3px;">' + data.token + '</code></div>';
    } else {
        alert('Failed: ' + r.status);
    }
}
async function revokeToken(token) {
    if(!confirm('Revoke this token?')) return;
    const r = await fetch('/api/admin/tokens/' + encodeURIComponent(token), {method:'DELETE'});
    if(r.ok) location.reload(); else alert('Failed: ' + r.status);
}
</script>"#);

    html.push_str("</body></html>");
    Ok(Html(html))
}

#[derive(Deserialize)]
pub struct CreateTokenReq {
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct CreateTokenResp {
    pub token: String,
}

async fn api_list_tokens(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<db::ApiToken>>, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let tokens = db::list_api_tokens(&mut conn).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(tokens))
}

async fn api_create_token(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(req): Json<CreateTokenReq>,
) -> Result<Json<CreateTokenResp>, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let token = format!("wyd_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
    let mut conn = state.pool.lock().unwrap();
    db::add_api_token(&mut conn, &token, req.description.as_deref()).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(CreateTokenResp { token }))
}

async fn api_revoke_token(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<StatusCode, StatusCode> {
    check_api_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let ok = db::delete_api_token(&mut conn, &token).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
}
