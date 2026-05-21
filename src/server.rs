use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
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
    pub client_id: Option<String>,
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
    pub client_id: Option<String>,
}

#[derive(Deserialize)]
pub struct FilterQuery {
    pub client_id: Option<String>,
    pub alias: Option<String>,
    pub command: Option<String>,
}

#[derive(Serialize)]
pub struct SvgResp {
    pub all_time: String,
    pub years: Vec<(String, String)>,
}

#[derive(Deserialize)]
pub struct CreateTokenReq {
    pub client_id: Option<String>,
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct CreateTokenResp {
    pub token: String,
}

#[derive(Deserialize)]
pub struct ListRecordsQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub client_id: Option<String>,
    pub alias: Option<String>,
    pub command: Option<String>,
}

#[derive(Serialize)]
pub struct RecordPageItem {
    pub id: i64,
    pub client_id: String,
    pub alias: Option<String>,
    pub command: Option<String>,
    pub start_time: i64,
    pub end_time: Option<i64>,
    pub duration_seconds: Option<i64>,
}

#[derive(Serialize)]
pub struct RecordsPageResp {
    pub records: Vec<RecordPageItem>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}

pub fn create_router(state: AppState) -> Router {
    let api_routes = Router::new()
        .route("/sessions/start", post(api_start_session))
        .route("/sessions/:id/end", post(api_end_session))
        .route("/sessions/:id/discard", post(api_discard_session))
        .route("/sessions/orphaned", get(api_get_orphaned))
        .route("/stats", get(api_get_stats))
        .route("/stats/by-client", get(api_get_stats_by_client))
        .route("/stats/by-alias", get(api_get_stats_by_alias))
        .route("/stats/by-command", get(api_get_stats_by_command))
        .route("/stats/session-distribution", get(api_get_session_distribution))
        .route("/stats/weekday-weekend", get(api_get_weekday_weekend_stats))
        .route("/stats/streaks", get(api_get_streaks))
        .route("/stats/monthly-trend", get(api_get_monthly_trend))
        .route("/stats/hourly-heatmap", get(api_get_hourly_heatmap))
        .route("/stats/summary.md", get(api_get_summary_md))
        .route("/daily-data", get(api_get_daily_data))
        .route("/svg", get(api_get_svg))
        .route("/records", get(api_list_records))
        .route("/clients", get(api_list_clients))
        .route("/aliases", get(api_get_aliases))
        .route("/admin/import", post(api_import))
        .route("/admin/backup", get(api_backup))
        .route("/admin/tokens", get(api_list_tokens).post(api_create_token))
        .route("/admin/tokens/:token", delete(api_revoke_token));

    let web_routes = Router::new()
        .route("/", get(web_index))
        .route("/admin", get(web_admin));

    Router::new()
        .nest("/api", api_routes)
        .merge(web_routes)
        .with_state(state)
}

/// Try API Bearer auth first; fall back to Web Basic auth as admin.
/// Returns (is_admin, client_id).
async fn resolve_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(bool, String), Response> {
    if let Ok(client_id) = check_api_auth(state, headers).await {
        Ok((false, client_id))
    } else if check_web_auth(state, headers).await.is_ok() {
        Ok((true, "__global__".to_string()))
    } else {
        Err(unauthorized_web_response())
    }
}

fn unauthorized_web_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"wyd-admin\"")],
        "Unauthorized",
    ).into_response()
}

async fn api_start_session(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(req): Json<StartSessionReq>,
) -> Result<Json<StartSessionResp>, StatusCode> {
    let token_client_id = check_api_auth(&state, &headers).await?;
    let client_id = req.client_id.unwrap_or(token_client_id);
    let mut conn = state.pool.lock().unwrap();
    let id = db::start_session(
        &mut conn,
        &client_id,
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
) -> Result<Json<EndSessionResp>, Response> {
    let (is_admin, client_id) = resolve_auth(&state, &headers).await?;
    let mut conn = state.pool.lock().unwrap();
    let duration = if is_admin {
        db::end_session_admin(&mut conn, id)
    } else {
        db::end_session(&mut conn, id, &client_id)
    }.map_err(|e| {
        tracing::error!("DB error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;

    match duration {
        Some(d) => Ok(Json(EndSessionResp { session_id: id, duration_seconds: d })),
        None => Err((StatusCode::NOT_FOUND, "Not Found").into_response()),
    }
}

async fn api_discard_session(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let mut conn = state.pool.lock().unwrap();
    let ok = db::discard_session_admin(&mut conn, id).map_err(|e| {
        tracing::error!("DB error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;

    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "Not Found").into_response())
    }
}

async fn api_get_orphaned(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<OrphanedSession>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let mut conn = state.pool.lock().unwrap();
    let records = db::get_orphaned_sessions_admin(&mut conn).map_err(|e| {
        tracing::error!("DB error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
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
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<stats::Stats>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let stats = compute_stats(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Stats error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(stats))
}

async fn api_get_stats_by_client(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<stats::ClientStat>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_stats_by_client(&mut conn, &client_id).map_err(|e| {
        tracing::error!("Stats error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_stats_by_alias(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<stats::AliasStat>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_stats_by_alias(&mut conn, &client_id).map_err(|e| {
        tracing::error!("Stats error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_stats_by_command(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<stats::CommandStat>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_stats_by_command(&mut conn, &client_id).map_err(|e| {
        tracing::error!("Stats error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_session_distribution(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<stats::SessionDistribution>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_session_distribution(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Session distribution error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_weekday_weekend_stats(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<stats::WeekdayWeekendStats>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_weekday_weekend_stats(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Weekday/weekend stats error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_streaks(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<stats::StreakStats>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_streaks(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Streaks error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_monthly_trend(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<stats::MonthlyPoint>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_monthly_trend(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Monthly trend error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_hourly_heatmap(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<stats::HourlyHeatmap>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_hourly_heatmap(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Hourly heatmap error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_summary_md(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let stats = compute_stats(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Stats error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;

    let daily = stats::get_daily_data(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Daily data error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
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
) -> Result<Json<serde_json::Value>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = req.client_id.unwrap_or_else(|| "default".to_string());
    let result = crate::importer::import_from_directory(&state.pool, &client_id, &req.path).await.map_err(|e| {
        tracing::error!("Import error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
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
) -> Result<Response, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let temp_path = format!("/tmp/wyd_backup_{}.db", chrono::Utc::now().timestamp());
    {
        let conn = state.pool.lock().unwrap();
        let mut dest = rusqlite::Connection::open(&temp_path).map_err(|e| {
            tracing::error!("Backup temp db error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        })?;
        let backup = rusqlite::backup::Backup::new(&*conn, &mut dest).map_err(|e| {
            tracing::error!("Backup init error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        })?;
        backup.run_to_completion(5, std::time::Duration::from_millis(100), None).map_err(|e| {
            tracing::error!("Backup run error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        })?;
    }

    let data = tokio::fs::read(&temp_path).await.map_err(|e| {
        tracing::error!("Read backup file error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;

    let _ = tokio::fs::remove_file(&temp_path).await;

    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"wyd-backup.db\""),
        ],
        data,
    ).into_response())
}

const INDEX_HTML: &str = r##"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>WYD Stats</title>
<style>
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;margin:0;padding:20px 40px;color:#333;background:#f6f8fa}
.card{background:#fff;border-radius:8px;padding:20px;margin-bottom:20px;box-shadow:0 1px 3px rgba(0,0,0,0.1)}
h1,h2{margin-top:0}
.stats-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:15px;margin-bottom:20px}
.stat-card{background:#fff;border-radius:8px;padding:15px;text-align:center;box-shadow:0 1px 3px rgba(0,0,0,0.1)}
.stat-value{font-size:24px;font-weight:bold;color:#0969da}
.stat-label{font-size:12px;color:#666;margin-top:5px}
.interval-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:15px}
.interval-card{background:#fff;border-radius:8px;padding:18px;text-align:center;box-shadow:0 1px 3px rgba(0,0,0,0.08);border-top:4px solid #0969da}
.interval-card.max{border-top-color:#cf222e}
.interval-card.mean{border-top-color:#2da44e}
.interval-icon{font-size:22px;margin-bottom:6px}
.interval-value{font-size:28px;font-weight:bold;color:#333}
.interval-unit{font-size:13px;color:#666}
.interval-bar-bg{background:#ebedf0;border-radius:4px;height:8px;margin-top:10px;overflow:hidden}
.interval-bar-fill{height:100%;border-radius:4px;background:#0969da;transition:width .3s}
.interval-card.max .interval-bar-fill{background:#cf222e}
.interval-card.mean .interval-bar-fill{background:#2da44e}
table{width:100%;border-collapse:collapse;margin-top:10px}
th,td{padding:8px 12px;text-align:left;border-bottom:1px solid #eee}
th{font-size:12px;color:#666;font-weight:600}
.col-dur{white-space:nowrap;min-width:110px;text-align:right;font-variant-numeric:tabular-nums}
.nav{margin-bottom:20px}
.nav a{color:#0969da;text-decoration:none;margin-right:15px}
.filters{display:flex;gap:10px;flex-wrap:wrap;margin-bottom:15px;align-items:center;}
.filters select,.filters input{padding:6px 10px;border-radius:4px;border:1px solid #d0d7de;}
.btn{padding:6px 12px;border:none;border-radius:4px;cursor:pointer;font-size:12px;margin-right:5px}
.btn-gen{background:#0969da;color:#fff}
.code{font-family:monospace;background:#f6f8fa;padding:2px 6px;border-radius:3px;font-size:12px}
#svg-all-time{margin-top:10px}
.year-svg{margin-top:10px}
.overview-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px}
.overview-grid .stat-card{box-shadow:none;border:1px solid #eee;padding:12px}
.pattern-grid{display:grid;grid-template-columns:2fr 1fr;gap:20px;align-items:start}
@media(max-width:900px){.pattern-grid{grid-template-columns:1fr}}
.year-graphs-wrap{margin-top:15px}
#year-graphs .card{margin-bottom:15px}
</style></head><body>
<div class="nav"><a href="/">Home</a><a href="/admin">Admin</a></div>
<h1>Activity Stats</h1>
<div class="card">
  <div class="filters">
    <select id="filter-client"><option value="">All Clients</option></select>
    <select id="filter-alias"><option value="">All Aliases</option></select>
    <input id="filter-command" type="text" placeholder="Filter command..." style="min-width:150px;">
    <select id="filter-perpage" style="padding:6px 10px;border-radius:4px;border:1px solid #d0d7de;">
      <option value="20">20 / page</option>
      <option value="50" selected>50 / page</option>
      <option value="100">100 / page</option>
      <option value="200">200 / page</option>
    </select>
    <button class="btn btn-gen" onclick="applyFilters()">Search</button>
  </div>
</div>
<div class="card">
  <h2>Overview</h2>
  <div id="overview-stats" class="overview-grid"></div>
</div>
<div class="card">
  <h2>Activity Graph (All Time)</h2>
  <div id="svg-all-time"></div>
  <div class="year-graphs-wrap">
    <button class="btn btn-gen" onclick="toggleYearGraphs()" id="year-toggle-btn" style="margin-bottom:10px;">Show Year Graphs ▼</button>
    <div id="year-graphs" style="display:none;"></div>
  </div>
</div>
<div class="card"><h2>Monthly Trend</h2><div id="monthly-trend"></div></div>
<div class="card">
  <h2>Activity Patterns</h2>
  <div class="pattern-grid">
    <div id="hourly-heatmap"></div>
    <div id="wd-we-stats"></div>
  </div>
</div>
<div class="card"><h2>Session Distribution</h2><div id="session-dist"></div></div>
<div class="card"><h2>Past N Stats</h2><table id="past-n-table"><thead><tr><th>Period</th><th class="col-dur">Duration</th><th>Ratio</th><th>Times</th><th>Day Ratio</th><th class="col-dur">Mean</th></tr></thead><tbody></tbody></table></div>
<div class="card" id="card-by-client"><h2>Stats by Client</h2><div id="client-stats"></div></div>
<div class="card" id="card-by-alias"><h2>Stats by Alias</h2><div id="alias-stats"></div></div>
<div class="card" id="card-by-command"><h2>Stats by Command</h2><div id="command-stats"></div></div>
<div class="card"><h2>Records</h2>
<div id="records-table"></div>
<div id="records-paging" style="margin-top:15px;display:flex;gap:10px;align-items:center;flex-wrap:wrap;"></div>
</div>
<script>
let recState = {page:1, perPage:50};
function getFilters() {
  return {
    client_id: document.getElementById('filter-client').value,
    alias: document.getElementById('filter-alias').value,
    command: document.getElementById('filter-command').value.trim()
  };
}
function buildParams(obj) {
  const f = getFilters();
  const p = new URLSearchParams();
  for(const k in obj) p.set(k, String(obj[k]));
  if(f.client_id) p.set('client_id', f.client_id);
  if(f.alias) p.set('alias', f.alias);
  if(f.command) p.set('command', f.command);
  return p;
}
function readUrlFilters() {
  const params = new URLSearchParams(location.search);
  const client_id = params.get('client_id') || '';
  const alias = params.get('alias') || '';
  const command = params.get('command') || '';
  const per_page = params.get('per_page') || '';
  if(client_id) document.getElementById('filter-client').value = client_id;
  if(alias) document.getElementById('filter-alias').value = alias;
  if(command) document.getElementById('filter-command').value = command;
  if(per_page) document.getElementById('filter-perpage').value = per_page;
}
function writeUrlFilters() {
  const f = getFilters();
  const params = new URLSearchParams();
  if(f.client_id) params.set('client_id', f.client_id);
  if(f.alias) params.set('alias', f.alias);
  if(f.command) params.set('command', f.command);
  const per_page = document.getElementById('filter-perpage').value;
  if(per_page && per_page !== '50') params.set('per_page', per_page);
  const qs = params.toString();
  history.replaceState(null, '', qs ? '?' + qs : location.pathname);
}
async function applyFilters() {
  recState.page = 1;
  writeUrlFilters();
  await loadAll();
}
function updateGroupStatsVisibility() {
  const f = getFilters();
  const hasFilter = !!(f.client_id || f.alias || f.command);
  const d = hasFilter ? 'none' : '';
  document.getElementById('card-by-client').style.display = d;
  document.getElementById('card-by-alias').style.display = d;
  document.getElementById('card-by-command').style.display = d;
}
async function loadAll() {
  updateGroupStatsVisibility();
  document.getElementById('overview-stats').innerHTML = '';
  await loadStats();
  await loadStreaks();
  await Promise.all([
    loadSvg(),
    loadClientStats(),
    loadAliasStats(),
    loadCommandStats(),
    loadSessionDistribution(),
    loadWeekdayWeekendStats(),
    loadMonthlyTrend(),
    loadHourlyHeatmap(),
    loadRecords()
  ]);
}
async function loadClients() {
  const r = await fetch('/api/clients');
  if(!r.ok) return;
  const data = await r.json();
  const sel = document.getElementById('filter-client');
  data.forEach(c => {
    const opt = document.createElement('option');
    opt.value = c; opt.textContent = c;
    sel.appendChild(opt);
  });
}
async function loadAliases() {
  const r = await fetch('/api/aliases');
  if(!r.ok) return;
  const data = await r.json();
  const sel = document.getElementById('filter-alias');
  data.forEach(a => {
    const opt = document.createElement('option');
    opt.value = a; opt.textContent = a;
    sel.appendChild(opt);
  });
}
function fmtDate(ts) {
  if(!ts) return '-';
  const d = new Date(ts*1000);
  return d.toISOString().slice(0,19).replace('T',' ');
}
function fmtDur(s) {
  if(s==null) return '-';
  const h=Math.floor(s/3600), m=Math.floor((s%3600)/60), sec=s%60;
  let out='';
  if(h>0) out+=String(h).padStart(2,'0')+' h ';
  if(m>0 || h>0) out+=String(m).padStart(2,'0')+' m ';
  out+=String(sec).padStart(2,'0')+' s';
  return out.trim();
}
async function loadStats() {
  const r = await fetch('/api/stats?' + buildParams({}).toString());
  if(!r.ok) return;
  const s = await r.json();
  const maxInt = s.interval.max_interval || 1;
  const curPct = Math.min(100, Math.round((s.interval.current_interval / maxInt) * 100));
  const meanPct = Math.min(100, Math.round((s.interval.mean_interval / maxInt) * 100));
  document.getElementById('overview-stats').innerHTML =
    '<div class="stat-card"><div class="stat-value">' + s.total.total_days + '</div><div class="stat-label">Total Days</div></div>' +
    '<div class="stat-card"><div class="stat-value">' + s.total.total_times + '</div><div class="stat-label">Total Times</div></div>' +
    '<div class="stat-card"><div class="stat-value">' + s.total.total_seconds_hr + '</div><div class="stat-label">Total Duration</div></div>' +
    '<div class="stat-card"><div class="stat-value">' + s.total.mean_usage_hr + '</div><div class="stat-label">Mean / Session</div></div>' +
    '<div class="stat-card" style="border-top:3px solid #0969da;">' +
    '<div style="font-size:18px;margin-bottom:2px;">🔥</div>' +
    '<div class="stat-value" style="font-size:20px;">' + s.interval.current_interval_hr + '</div>' +
    '<div class="stat-label">Current Interval</div>' +
    '<div class="interval-bar-bg" style="margin-top:6px;"><div class="interval-bar-fill" style="width:' + curPct + '%"></div></div>' +
    '</div>' +
    '<div class="stat-card" style="border-top:3px solid #cf222e;">' +
    '<div style="font-size:18px;margin-bottom:2px;">📊</div>' +
    '<div class="stat-value" style="font-size:20px;">' + s.interval.max_interval_hr + '</div>' +
    '<div class="stat-label">Max Interval</div>' +
    '<div class="interval-bar-bg" style="margin-top:6px;"><div class="interval-bar-fill" style="width:100%;background:#cf222e;"></div></div>' +
    '</div>' +
    '<div class="stat-card" style="border-top:3px solid #2da44e;">' +
    '<div style="font-size:18px;margin-bottom:2px;">⏱</div>' +
    '<div class="stat-value" style="font-size:20px;">' + s.interval.mean_interval_hr + '</div>' +
    '<div class="stat-label">Mean Interval</div>' +
    '<div class="interval-bar-bg" style="margin-top:6px;"><div class="interval-bar-fill" style="width:' + meanPct + '%;background:#2da44e;"></div></div>' +
    '</div>';
  const avgRatio = s.past_n.reduce((sum, p) => sum + p.ratio, 0) / (s.past_n.length || 1);
  const avgDayRatio = s.past_n.reduce((sum, p) => sum + p.day_ratio, 0) / (s.past_n.length || 1);
  let html = '';
  s.past_n.forEach(p => {
    const ratioColor = p.ratio >= avgRatio ? '#2da44e' : '#666';
    const dayRatioColor = p.day_ratio >= avgDayRatio ? '#2da44e' : '#666';
    html += '<tr><td>' + p.name + '</td><td class="col-dur">' + fmtDur(p.seconds) + '</td><td style="color:' + ratioColor + '">' + p.ratio.toFixed(2) + '%</td><td>' + p.times + '</td><td style="color:' + dayRatioColor + '">' + p.day_ratio.toFixed(2) + '%</td><td class="col-dur">' + fmtDur(p.mean_usage) + '</td></tr>';
  });
  document.querySelector('#past-n-table tbody').innerHTML = html;
}
function toggleYearGraphs() {
  const el = document.getElementById('year-graphs');
  const btn = document.getElementById('year-toggle-btn');
  if(el.style.display === 'none') {
    el.style.display = '';
    btn.textContent = 'Hide Year Graphs ▲';
  } else {
    el.style.display = 'none';
    btn.textContent = 'Show Year Graphs ▼';
  }
}
async function loadSvg() {
  const r = await fetch('/api/svg?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  document.getElementById('svg-all-time').innerHTML = data.all_time;
  let yhtml = '';
  data.years.forEach(y => {
    yhtml += '<div class="card"><h2>' + y[0] + '</h2><div class="year-svg">' + y[1] + '</div></div>';
  });
  document.getElementById('year-graphs').innerHTML = yhtml;
}
async function loadClientStats() {
  const r = await fetch('/api/stats/by-client?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  let html = '<table><tr><th>Client</th><th>Total</th><th>Times</th><th>Mean</th></tr>';
  if(data.length===0) {
    html += '<tr><td colspan="4" style="text-align:center;color:#666;">No data</td></tr>';
  } else {
    data.forEach(s => {
      html += '<tr><td>' + s.client_id + '</td><td>' + s.total_seconds_hr + '</td><td>' + s.total_times + '</td><td>' + s.mean_seconds_hr + '</td></tr>';
    });
  }
  html += '</table>';
  document.getElementById('client-stats').innerHTML = html;
}
async function loadAliasStats() {
  const r = await fetch('/api/stats/by-alias?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  let html = '<table><tr><th>Alias</th><th>Total</th><th>Times</th><th>Mean</th></tr>';
  if(data.length===0) {
    html += '<tr><td colspan="4" style="text-align:center;color:#666;">No data</td></tr>';
  } else {
    data.forEach(s => {
      html += '<tr><td>' + s.alias + '</td><td>' + s.total_seconds_hr + '</td><td>' + s.total_times + '</td><td>' + s.mean_seconds_hr + '</td></tr>';
    });
  }
  html += '</table>';
  document.getElementById('alias-stats').innerHTML = html;
}
async function loadCommandStats() {
  const r = await fetch('/api/stats/by-command?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  let html = '<table><tr><th>Command</th><th>Total</th><th>Times</th><th>Mean</th></tr>';
  if(data.length===0) {
    html += '<tr><td colspan="4" style="text-align:center;color:#666;">No data</td></tr>';
  } else {
    data.forEach(s => {
      html += '<tr><td class="code">' + (s.command||'-') + '</td><td>' + s.total_seconds_hr + '</td><td>' + s.total_times + '</td><td>' + s.mean_seconds_hr + '</td></tr>';
    });
  }
  html += '</table>';
  document.getElementById('command-stats').innerHTML = html;
}
async function loadSessionDistribution() {
  const r = await fetch('/api/stats/session-distribution?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  if(data.total_sessions === 0) {
    document.getElementById('session-dist').innerHTML = '<p style="color:#666;">No data</p>';
    return;
  }
  let html = '<div style="display:grid;grid-template-columns:repeat(4,1fr);gap:10px;margin-bottom:12px;">';
  html += '<div class="stat-card"><div class="stat-value">' + data.max_seconds_hr + '</div><div class="stat-label">Max</div></div>';
  html += '<div class="stat-card"><div class="stat-value">' + data.min_seconds_hr + '</div><div class="stat-label">Min</div></div>';
  html += '<div class="stat-card"><div class="stat-value">' + data.median_seconds_hr + '</div><div class="stat-label">Median</div></div>';
  html += '<div class="stat-card"><div class="stat-value">' + data.mean_seconds_hr + '</div><div class="stat-label">Mean</div></div>';
  html += '</div>';
  html += '<div style="display:flex;flex-direction:column;gap:6px;">';
  data.buckets.forEach(b => {
    html += '<div style="display:flex;align-items:center;gap:8px;font-size:12px;">';
    html += '<span style="width:70px;color:#666;">' + b.label + '</span>';
    html += '<div style="flex:1;background:#ebedf0;border-radius:4px;height:16px;overflow:hidden;">';
    html += '<div style="width:' + Math.min(100, b.pct.toFixed(1)) + '%;height:100%;background:#0969da;border-radius:4px;"></div>';
    html += '</div>';
    html += '<span style="width:70px;text-align:right;color:#333;">' + b.count + ' (' + b.pct.toFixed(1) + '%)</span>';
    html += '</div>';
  });
  html += '</div>';
  document.getElementById('session-dist').innerHTML = html;
}
async function loadWeekdayWeekendStats() {
  const r = await fetch('/api/stats/weekday-weekend?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  if(data.weekday_times === 0 && data.weekend_times === 0) {
    document.getElementById('wd-we-stats').innerHTML = '<p style="color:#666;">No data</p>';
    return;
  }
  const maxTotal = Math.max(data.weekday_total_seconds, data.weekend_total_seconds, 1);
  const maxTimes = Math.max(data.weekday_times, data.weekend_times, 1);
  const maxMean = Math.max(data.weekday_mean_seconds, data.weekend_mean_seconds, 1);
  const barMax = 130;
  function bar(val, max) { return Math.max(2, Math.round(val / max * barMax)); }
  let svg = '<svg width="100%" height="170" viewBox="0 0 260 170" xmlns="http://www.w3.org/2000/svg">';
  svg += '<text x="5" y="18" font-size="11" fill="#333" font-weight="600">Total</text>';
  svg += '<rect x="55" y="8" width="' + bar(data.weekday_total_seconds, maxTotal) + '" height="12" fill="#0969da" rx="2"/>';
  svg += '<text x="' + (60 + bar(data.weekday_total_seconds, maxTotal)) + '" y="18" font-size="9" fill="#666">' + data.weekday_total_hr + '</text>';
  svg += '<rect x="55" y="22" width="' + bar(data.weekend_total_seconds, maxTotal) + '" height="12" fill="#2da44e" rx="2"/>';
  svg += '<text x="' + (60 + bar(data.weekend_total_seconds, maxTotal)) + '" y="32" font-size="9" fill="#666">' + data.weekend_total_hr + '</text>';
  svg += '<text x="5" y="58" font-size="11" fill="#333" font-weight="600">Times</text>';
  svg += '<rect x="55" y="48" width="' + bar(data.weekday_times, maxTimes) + '" height="12" fill="#0969da" rx="2"/>';
  svg += '<text x="' + (60 + bar(data.weekday_times, maxTimes)) + '" y="58" font-size="9" fill="#666">' + data.weekday_times + '</text>';
  svg += '<rect x="55" y="62" width="' + bar(data.weekend_times, maxTimes) + '" height="12" fill="#2da44e" rx="2"/>';
  svg += '<text x="' + (60 + bar(data.weekend_times, maxTimes)) + '" y="72" font-size="9" fill="#666">' + data.weekend_times + '</text>';
  svg += '<text x="5" y="98" font-size="11" fill="#333" font-weight="600">Mean</text>';
  svg += '<rect x="55" y="88" width="' + bar(data.weekday_mean_seconds, maxMean) + '" height="12" fill="#0969da" rx="2"/>';
  svg += '<text x="' + (60 + bar(data.weekday_mean_seconds, maxMean)) + '" y="98" font-size="9" fill="#666">' + data.weekday_mean_hr + '</text>';
  svg += '<rect x="55" y="102" width="' + bar(data.weekend_mean_seconds, maxMean) + '" height="12" fill="#2da44e" rx="2"/>';
  svg += '<text x="' + (60 + bar(data.weekend_mean_seconds, maxMean)) + '" y="112" font-size="9" fill="#666">' + data.weekend_mean_hr + '</text>';
  svg += '<rect x="55" y="140" width="10" height="10" fill="#0969da" rx="2"/><text x="69" y="149" font-size="10" fill="#666">Weekday</text>';
  svg += '<rect x="135" y="140" width="10" height="10" fill="#2da44e" rx="2"/><text x="149" y="149" font-size="10" fill="#666">Weekend</text>';
  svg += '</svg>';
  document.getElementById('wd-we-stats').innerHTML = svg;
}
async function loadStreaks() {
  const r = await fetch('/api/stats/streaks?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  const html =
    '<div class="stat-card"><div class="stat-value">' + data.current_streak + '</div><div class="stat-label">Current Streak</div></div>' +
    '<div class="stat-card"><div class="stat-value">' + data.max_streak + '</div><div class="stat-label">Max Streak</div></div>' +
    '<div class="stat-card"><div class="stat-value" style="font-size:16px;">' + data.last_active_date + '</div><div class="stat-label">Last Active</div></div>';
  document.getElementById('overview-stats').insertAdjacentHTML('beforeend', html);
}
async function loadMonthlyTrend() {
  const r = await fetch('/api/stats/monthly-trend?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  if(data.length === 0) {
    document.getElementById('monthly-trend').innerHTML = '<p style="color:#666;">No data</p>';
    return;
  }
  const w = 800, h = 180, pad = 30;
  const maxSec = Math.max(...data.map(d => d.total_seconds), 1);
  const maxTimes = Math.max(...data.map(d => d.total_times), 1);
  const stepX = data.length > 1 ? (w - pad * 2) / (data.length - 1) : 0;
  let points = '', timePoints = '';
  data.forEach((d, i) => {
    const x = pad + i * stepX;
    const y = h - pad - (d.total_seconds / maxSec) * (h - pad * 2);
    const yTimes = h - pad - (d.total_times / maxTimes) * (h - pad * 2);
    points += x + ',' + y + ' ';
    timePoints += x + ',' + yTimes + ' ';
  });
  let svg = '<svg width="100%" height="' + h + '" viewBox="0 0 ' + w + ' ' + h + '" xmlns="http://www.w3.org/2000/svg">';
  svg += '<polyline points="' + points.trim() + '" fill="none" stroke="#0969da" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>';
  svg += '<polyline points="' + timePoints.trim() + '" fill="none" stroke="#2da44e" stroke-width="1.5" stroke-dasharray="4,4" stroke-linecap="round" stroke-linejoin="round"/>';
  const labelStep = Math.max(1, Math.floor(data.length / 10));
  data.forEach((d, i) => {
    const x = pad + i * stepX;
    const y = h - pad - (d.total_seconds / maxSec) * (h - pad * 2);
    const yTimes = h - pad - (d.total_times / maxTimes) * (h - pad * 2);
    svg += '<circle cx="' + x + '" cy="' + y + '" r="3" fill="#0969da"/>';
    svg += '<circle cx="' + x + '" cy="' + yTimes + '" r="2" fill="#2da44e"/>';
    svg += '<title>' + d.year_month + ': ' + d.total_seconds_hr + ' (' + d.total_times + ' times)</title>';
    if(i % labelStep === 0) {
      svg += '<text x="' + x + '" y="' + (h - 5) + '" font-size="10" fill="#666" text-anchor="middle">' + d.year_month + '</text>';
    }
  });
  svg += '<text x="' + pad + '" y="16" font-size="10" fill="#0969da" text-anchor="middle">● Duration</text>';
  svg += '<text x="' + (pad + 80) + '" y="16" font-size="10" fill="#2da44e" text-anchor="middle">- - Times</text>';
  svg += '</svg>';
  document.getElementById('monthly-trend').innerHTML = svg;
}
async function loadHourlyHeatmap() {
  const r = await fetch('/api/stats/hourly-heatmap?' + buildParams({}).toString());
  if(!r.ok) return;
  const data = await r.json();
  const cell = 24, gap = 2, labelW = 60;
  const w = labelW + 24 * (cell + gap) + 20;
  const h = 40 + 7 * (cell + gap) + 20;
  const days = ['Mon','Tue','Wed','Thu','Fri','Sat','Sun'];
  let svg = '<svg width="100%" height="' + h + '" viewBox="0 0 ' + w + ' ' + h + '" xmlns="http://www.w3.org/2000/svg">';
  days.forEach((d, i) => {
    svg += '<text x="8" y="' + (40 + i * (cell + gap) + cell / 2 + 5) + '" font-size="14" fill="#666">' + d + '</text>';
  });
  for(let hour = 0; hour < 24; hour++) {
    if(hour % 4 === 0) {
      svg += '<text x="' + (labelW + hour * (cell + gap) + cell / 2) + '" y="28" font-size="14" fill="#666" text-anchor="middle">' + hour + '</text>';
    }
  }
  data.grid.forEach((row, dow) => {
    row.forEach((seconds, hour) => {
      const x = labelW + hour * (cell + gap);
      const y = 40 + dow * (cell + gap);
      let color = '#ebedf0';
      if(seconds > 0 && data.max_seconds > 0) {
        const ratio = seconds / data.max_seconds;
        if(ratio < 0.33) color = '#9be9a8';
        else if(ratio < 0.66) color = '#f9d71c';
        else color = '#e5534b';
      }
      const tooltip = days[dow] + ' ' + hour + ':00: ' + fmtDur(seconds);
      svg += '<rect x="' + x + '" y="' + y + '" width="' + cell + '" height="' + cell + '" fill="' + color + '" rx="4"><title>' + tooltip + '</title></rect>';
    });
  });
  svg += '</svg>';
  document.getElementById('hourly-heatmap').innerHTML = svg;
}
async function loadRecords() {
  recState.perPage = parseInt(document.getElementById('filter-perpage').value);
  const params = buildParams({page: recState.page, per_page: recState.perPage});
  const r = await fetch('/api/records?' + params.toString());
  if(!r.ok) { document.getElementById('records-table').innerHTML = '<p>Error: ' + r.status + '</p>'; return; }
  const data = await r.json();
  let html = '<table><tr><th>ID</th><th>Client</th><th>Alias</th><th>Command</th><th>Start</th><th>End</th><th class="col-dur">Duration</th></tr>';
  if(data.records.length===0) {
    html += '<tr><td colspan="7" style="text-align:center;color:#666;">No records</td></tr>';
  } else {
    data.records.forEach(rec => {
      html += '<tr><td>' + rec.id + '</td><td>' + (rec.client_id||'-') + '</td><td>' + (rec.alias||'-') + '</td><td class="code">' + (rec.command||'-') + '</td><td>' + fmtDate(rec.start_time) + '</td><td>' + fmtDate(rec.end_time) + '</td><td class="col-dur">' + fmtDur(rec.duration_seconds) + '</td></tr>';
    });
  }
  html += '</table>';
  document.getElementById('records-table').innerHTML = html;
  let pg = '';
  pg += '<span style="color:#666;font-size:12px;">Total: ' + data.total + ' | Page ' + data.page + ' / ' + data.total_pages + '</span>';
  if(data.total_pages > 1) {
    pg += '<span style="display:flex;gap:5px;">';
    if(data.page > 1) pg += '<button class="btn" onclick="goPage(' + (data.page-1) + ')">Prev</button>';
    let start = Math.max(1, data.page - 3);
    let end = Math.min(data.total_pages, data.page + 3);
    for(let i=start;i<=end;i++) {
      if(i===data.page) pg += '<button class="btn" style="background:#0969da;color:#fff;">' + i + '</button>';
      else pg += '<button class="btn" onclick="goPage(' + i + ')">' + i + '</button>';
    }
    if(data.page < data.total_pages) pg += '<button class="btn" onclick="goPage(' + (data.page+1) + ')">Next</button>';
    pg += '</span>';
  }
  document.getElementById('records-paging').innerHTML = pg;
}
function goPage(p) { recState.page = p; loadRecords(); }
document.getElementById('filter-command').addEventListener('keypress', e => { if(e.key==='Enter') { applyFilters(); } });
loadClients().then(() => loadAliases().then(() => {
  readUrlFilters();
  loadAll();
}));
</script>
</body></html>"##;

async fn web_index(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Html<String>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    Ok(Html(INDEX_HTML.to_string()))
}

async fn web_admin(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Html<String>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let mut conn = state.pool.lock().unwrap();
    let orphaned = match db::get_orphaned_sessions_admin(&mut conn) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("DB error: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response());
        }
    };

    let tokens = match db::list_api_tokens(&mut conn) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("DB error: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response());
        }
    };

    let now = chrono::Utc::now().timestamp();

    let mut html = String::new();
    html.push_str(r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>WYD Admin</title>
<style>
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;margin:0;padding:20px 40px;color:#333;background:#f6f8fa}
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
    const clientId = prompt('Client ID (leave empty for default):') || undefined;
    const body = clientId ? {description: desc, client_id: clientId} : {description: desc};
    const r = await fetch('/api/admin/tokens', {
        method:'POST',
        headers:{'Content-Type':'application/json'},
        body: JSON.stringify(body)
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

async fn api_list_records(
    Query(q): Query<ListRecordsQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<RecordsPageResp>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_filter = q.client_id.unwrap_or_default();
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(50).clamp(1, 500);
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();

    let mut conn = state.pool.lock().unwrap();
    let (records, total) = db::list_records_page(&mut conn, &client_filter, &alias, &command, page, per_page)
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        })?;

    let total_pages = (total + per_page - 1) / per_page;
    let items: Vec<RecordPageItem> = records.into_iter().map(|r| RecordPageItem {
        id: r.id,
        client_id: r.client_id,
        alias: r.alias,
        command: r.command,
        start_time: r.start_time,
        end_time: r.end_time,
        duration_seconds: r.duration_seconds,
    }).collect();

    Ok(Json(RecordsPageResp {
        records: items,
        total,
        page,
        per_page,
        total_pages,
    }))
}

async fn api_list_clients(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<String>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let mut conn = state.pool.lock().unwrap();
    let clients = db::distinct_client_ids(&mut conn).map_err(|e| {
        tracing::error!("DB error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(clients))
}

async fn api_list_tokens(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<db::ApiToken>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let mut conn = state.pool.lock().unwrap();
    let tokens = db::list_api_tokens(&mut conn).map_err(|e| {
        tracing::error!("DB error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(tokens))
}

async fn api_create_token(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(req): Json<CreateTokenReq>,
) -> Result<Json<CreateTokenResp>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = req.client_id.unwrap_or_else(|| "default".to_string());
    let token = format!("wyd_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
    let mut conn = state.pool.lock().unwrap();
    db::add_api_token(&mut conn, &token, &client_id, req.description.as_deref()).map_err(|e| {
        tracing::error!("DB error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(CreateTokenResp { token }))
}

async fn api_revoke_token(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<StatusCode, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let mut conn = state.pool.lock().unwrap();
    let ok = db::delete_api_token(&mut conn, &token).map_err(|e| {
        tracing::error!("DB error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "Not Found").into_response())
    }
}

async fn api_get_daily_data(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<(String, i64)>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let data = stats::get_daily_data(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Daily data error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    Ok(Json(data))
}

async fn api_get_svg(
    Query(q): Query<FilterQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<SvgResp>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let client_id = q.client_id.unwrap_or_else(|| "__global__".to_string());
    let alias = q.alias.unwrap_or_default();
    let command = q.command.unwrap_or_default();
    let mut conn = state.pool.lock().unwrap();
    let daily = stats::get_daily_data(&mut conn, &client_id, &alias, &command).map_err(|e| {
        tracing::error!("Daily data error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    let all_time = generate_svg_calendar(&daily, None);
    let years = generate_all_years_svgs(&daily);
    Ok(Json(SvgResp { all_time, years }))
}

async fn api_get_aliases(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<String>>, Response> {
    if check_web_auth(&state, &headers).await.is_err() {
        return Err(unauthorized_web_response());
    }
    let mut conn = state.pool.lock().unwrap();
    let data = stats::compute_stats_by_alias(&mut conn, "__global__").map_err(|e| {
        tracing::error!("Stats error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    })?;
    let aliases: Vec<String> = data.into_iter().map(|s| s.alias).collect();
    Ok(Json(aliases))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
}
