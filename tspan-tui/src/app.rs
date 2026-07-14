use crate::api_types::{
    human_readable_time, AliasStat, ClientStat, CommandStat, CreateTokenReq, CreateTokenResp,
    EndSessionResp, HourlyHeatmap, MonthlyPoint, OrphanedSession, RecordPageItem, RecordsPageResp,
    SessionDistribution, Stats, StreakStats, WeekdayWeekendStats,
};
use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, Utc};
use chrono_tz::Tz;
use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState, Tabs, Wrap,
    },
    Frame, Terminal,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{self, BufWriter, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const GLOBAL_CLIENT: &str = "__global__";

pub struct TuiOptions {
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub initial_client_id: String,
    pub initial_alias: String,
    pub timezone: String,
    pub page_size: u16,
    pub verbose_log: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Overview,
    Breakdown,
    Records,
    Active,
    Tokens,
    Analytics,
}

impl View {
    const ALL: [Self; 6] = [
        Self::Overview,
        Self::Breakdown,
        Self::Records,
        Self::Active,
        Self::Tokens,
        Self::Analytics,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Breakdown => "Stats",
            Self::Records => "Records",
            Self::Active => "Active",
            Self::Tokens => "Tokens",
            Self::Analytics => "Graphs",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|view| *view == self).unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyticsKind {
    Calendar,
    Monthly,
    Hourly,
    Patterns,
}

impl AnalyticsKind {
    const ALL: [Self; 4] = [Self::Calendar, Self::Monthly, Self::Hourly, Self::Patterns];

    fn title(self) -> &'static str {
        match self {
            Self::Calendar => "Calendar",
            Self::Monthly => "Monthly trend",
            Self::Hourly => "Hourly heatmap",
            Self::Patterns => "Patterns",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakdownKind {
    Clients,
    Aliases,
    Commands,
    Distribution,
}

impl BreakdownKind {
    const ALL: [Self; 4] = [
        Self::Clients,
        Self::Aliases,
        Self::Commands,
        Self::Distribution,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Clients => "Clients",
            Self::Aliases => "Aliases",
            Self::Commands => "Commands",
            Self::Distribution => "Durations",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

struct OverviewData {
    stats: Stats,
    streaks: StreakStats,
    distribution: SessionDistribution,
    by_client: Vec<ClientStat>,
    by_alias: Vec<AliasStat>,
    by_command: Vec<CommandStat>,
}

struct AnalyticsData {
    daily: Vec<(String, i64)>,
    monthly: Vec<MonthlyPoint>,
    hourly: HourlyHeatmap,
    weekday_weekend: WeekdayWeekendStats,
}

#[derive(Debug, Deserialize)]
struct ApiToken {
    token: String,
    client_id: String,
    description: Option<String>,
    created_at: i64,
}

#[derive(Debug, Clone)]
enum AdminAction {
    DeleteRecord(i64),
    EndSession(i64),
    DiscardSession(i64),
    RevokeToken(String),
}

impl AdminAction {
    fn prompt(&self) -> String {
        match self {
            Self::DeleteRecord(id) => format!("Permanently delete record #{id}?"),
            Self::EndSession(id) => format!("End active session #{id} at the current time?"),
            Self::DiscardSession(id) => format!("Discard active session #{id}?"),
            Self::RevokeToken(token) => {
                format!("Revoke API token {}?", redact_token(token))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenField {
    ClientId,
    Description,
}

enum Overlay {
    Help,
    Confirm(AdminAction),
    TokenForm {
        field: TokenField,
        client_id: String,
        description: String,
    },
    TokenCreated(String),
}

struct Notice {
    text: String,
    is_error: bool,
    is_warning: bool,
}

struct ApiClient {
    agent: ureq::Agent,
    base_url: String,
    authorization: String,
    trace: Option<ApiTrace>,
}

impl ApiClient {
    fn new(
        server_url: &str,
        username: &str,
        password: &str,
        verbose_log: Option<&Path>,
    ) -> Result<Self> {
        let base_url = server_url.trim().trim_end_matches('/').to_string();
        anyhow::ensure!(
            base_url.starts_with("http://") || base_url.starts_with("https://"),
            "server URL must begin with http:// or https://"
        );
        anyhow::ensure!(!username.contains(':'), "username cannot contain ':'");
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(15)))
                .http_status_as_error(false)
                .build(),
        );
        let trace = verbose_log.map(ApiTrace::new).transpose()?;
        Ok(Self {
            agent,
            base_url,
            authorization: format!("Basic {encoded}"),
            trace,
        })
    }

    fn label(&self) -> &str {
        &self.base_url
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/api/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        action: &str,
    ) -> Result<T> {
        let url = self.endpoint(path);
        let mut request = self
            .agent
            .get(&url)
            .header("Authorization", &self.authorization);
        for (key, value) in query {
            request = request.query(key, value);
        }
        let display_url = display_url_with_query(&url, query);
        let response = self.read_response("GET", &display_url, action, || request.call())?;
        let body = response.success_body(action)?;
        serde_json::from_str(body)
            .with_context(|| format!("{action}: server returned invalid JSON"))
    }

    fn overview(&self, client_id: &str, alias: &str, timezone: Tz) -> Result<OverviewData> {
        let client = client_id.to_string();
        let alias = alias.to_string();
        let tz = timezone.to_string();
        let filters = [
            ("client_id", client.clone()),
            ("alias", alias.clone()),
            ("tz", tz),
        ];
        let grouped_filter = [("client_id", client.clone()), ("alias", alias.clone())];
        let command_filter = [
            ("client_id", client),
            ("alias", alias),
            ("depth", "1".to_string()),
        ];
        Ok(OverviewData {
            stats: self.get_json("stats", &filters, "load summary statistics")?,
            streaks: self.get_json("stats/streaks", &filters, "load streak statistics")?,
            distribution: self.get_json(
                "stats/session-distribution",
                &grouped_filter,
                "load session distribution",
            )?,
            by_client: self.get_json(
                "stats/by-client",
                &grouped_filter,
                "load client statistics",
            )?,
            by_alias: self.get_json("stats/by-alias", &grouped_filter, "load alias statistics")?,
            by_command: self.get_json(
                "stats/by-command",
                &command_filter,
                "load command statistics",
            )?,
        })
    }

    fn analytics(&self, client_id: &str, alias: &str, timezone: Tz) -> Result<AnalyticsData> {
        let filters = [
            ("client_id", client_id.to_string()),
            ("alias", alias.to_string()),
            ("tz", timezone.to_string()),
        ];
        Ok(AnalyticsData {
            daily: self.get_json("daily-data", &filters, "load activity calendar")?,
            monthly: self.get_json("stats/monthly-trend", &filters, "load monthly trend")?,
            hourly: self.get_json("stats/hourly-heatmap", &filters, "load hourly heatmap")?,
            weekday_weekend: self.get_json(
                "stats/weekday-weekend",
                &filters,
                "load weekday and weekend statistics",
            )?,
        })
    }

    fn clients(&self) -> Result<Vec<String>> {
        self.get_json("clients", &[], "load clients")
    }

    fn aliases(&self) -> Result<Vec<String>> {
        self.get_json("aliases", &[], "load aliases")
    }

    fn records(
        &self,
        client_id: &str,
        alias: &str,
        page: i64,
        page_size: i64,
    ) -> Result<RecordsPageResp> {
        self.get_json(
            "records",
            &[
                ("client_id", client_id.to_string()),
                ("alias", alias.to_string()),
                ("page", page.to_string()),
                ("per_page", page_size.to_string()),
            ],
            "load records",
        )
    }

    fn active_sessions(&self) -> Result<Vec<OrphanedSession>> {
        self.get_json("sessions/orphaned", &[], "load active sessions")
    }

    fn tokens(&self) -> Result<Vec<ApiToken>> {
        self.get_json("admin/tokens", &[], "load API tokens")
    }

    fn delete_record(&self, id: i64) -> Result<bool> {
        self.delete(&format!("admin/records/{id}"), "delete record")
    }

    fn end_session(&self, id: i64) -> Result<Option<i64>> {
        let url = self.endpoint(&format!("sessions/{id}/end"));
        let request = self
            .agent
            .post(&url)
            .header("Authorization", &self.authorization);
        let response = self.read_response("POST", &url, "end session", || request.send_empty())?;
        if response.status == 404 {
            return Ok(None);
        }
        let result: EndSessionResp = serde_json::from_str(response.success_body("end session")?)
            .context("end session: server returned invalid JSON")?;
        Ok(Some(result.duration_seconds))
    }

    fn discard_session(&self, id: i64) -> Result<bool> {
        self.post_empty(&format!("sessions/{id}/discard"), "discard session")
    }

    fn create_token(&self, client_id: &str, description: Option<&str>) -> Result<String> {
        let body = serde_json::to_string(&CreateTokenReq {
            client_id: Some(client_id.to_string()),
            description: description.map(str::to_string),
        })
        .context("create token: could not encode request")?;
        let url = self.endpoint("admin/tokens");
        let request = self
            .agent
            .post(&url)
            .header("Authorization", &self.authorization)
            .header("Content-Type", "application/json");
        let response = self.read_response("POST", &url, "create token", || request.send(body))?;
        let result: CreateTokenResp = serde_json::from_str(response.success_body("create token")?)
            .context("create token: server returned invalid JSON")?;
        Ok(result.token)
    }

    fn revoke_token(&self, token: &str) -> Result<bool> {
        self.delete(&format!("admin/tokens/{token}"), "revoke token")
    }

    fn delete(&self, path: &str, action: &str) -> Result<bool> {
        let url = self.endpoint(path);
        let request = self
            .agent
            .delete(&url)
            .header("Authorization", &self.authorization);
        let response = self.read_response("DELETE", &url, action, || request.call())?;
        if response.status == 404 {
            Ok(false)
        } else {
            response.success_body(action)?;
            Ok(true)
        }
    }

    fn post_empty(&self, path: &str, action: &str) -> Result<bool> {
        let url = self.endpoint(path);
        let request = self
            .agent
            .post(&url)
            .header("Authorization", &self.authorization);
        let response = self.read_response("POST", &url, action, || request.send_empty())?;
        if response.status == 404 {
            Ok(false)
        } else {
            response.success_body(action)?;
            Ok(true)
        }
    }

    fn read_response<F>(
        &self,
        method: &str,
        url: &str,
        action: &str,
        send: F,
    ) -> Result<ApiResponse>
    where
        F: FnOnce() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    {
        if let Some(trace) = self.trace.as_ref() {
            trace.request(method, url);
        }
        let mut response = match send() {
            Ok(response) => response,
            Err(error) => {
                if let Some(trace) = self.trace.as_ref() {
                    trace.transport_error(&error);
                }
                return Err(anyhow!("{action}: {error}"));
            }
        };
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .with_context(|| format!("{action}: could not read server response"))?;
        if let Some(trace) = self.trace.as_ref() {
            trace.response(status, &body);
        }
        Ok(ApiResponse { status, body })
    }
}

struct ApiTrace {
    writer: Mutex<BufWriter<File>>,
}

impl ApiTrace {
    fn new(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .with_context(|| format!("could not open verbose log '{}'", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .with_context(|| {
                    format!(
                        "could not secure verbose log permissions '{}'",
                        path.display()
                    )
                })?;
        }
        Ok(Self {
            writer: Mutex::new(BufWriter::new(file)),
        })
    }

    fn request(&self, method: &str, url: &str) {
        self.write(&format!("[tspan-tui] --> {method} {url}\n"));
    }

    fn transport_error(&self, error: &ureq::Error) {
        self.write(&format!("[tspan-tui] <-- transport error: {error}\n\n"));
    }

    fn response(&self, status: u16, body: &str) {
        self.write(&format!(
            "[tspan-tui] <-- HTTP {status}\n[tspan-tui] raw response body:\n{body}\n\n"
        ));
    }

    fn write(&self, entry: &str) {
        let Ok(mut writer) = self.writer.lock() else {
            return;
        };
        let _ = writer.write_all(entry.as_bytes());
        let _ = writer.flush();
    }
}

struct ApiResponse {
    status: u16,
    body: String,
}

impl ApiResponse {
    fn success_body(&self, action: &str) -> Result<&str> {
        match self.status {
            200..=299 => Ok(&self.body),
            401 => Err(anyhow!(
                "{action}: authentication failed (check --username and --password)"
            )),
            403 => Err(anyhow!("{action}: administrator access is required")),
            status => Err(anyhow!("{action}: server returned HTTP {status}")),
        }
    }
}

fn display_url_with_query(url: &str, query: &[(&str, String)]) -> String {
    if query.is_empty() {
        return url.to_string();
    }
    let query = query
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    format!("{url}?{query}")
}

struct App {
    api: ApiClient,
    timezone: Tz,
    view: View,
    breakdown_kind: BreakdownKind,
    analytics_kind: AnalyticsKind,
    calendar_offset_weeks: usize,
    breakdown_offset: usize,
    client_ids: Vec<String>,
    client_index: usize,
    aliases: Vec<String>,
    alias_index: usize,
    overview: Option<OverviewData>,
    analytics: Option<AnalyticsData>,
    records: Vec<RecordPageItem>,
    records_total: i64,
    records_page: i64,
    page_size: i64,
    records_state: TableState,
    active: Vec<OrphanedSession>,
    active_state: TableState,
    tokens: Vec<ApiToken>,
    tokens_state: TableState,
    overlay: Option<Overlay>,
    notice: Option<Notice>,
    legacy_records_api: bool,
    should_quit: bool,
    last_refresh: Instant,
}

impl App {
    fn new(options: TuiOptions) -> Result<Self> {
        let timezone = options
            .timezone
            .parse::<Tz>()
            .with_context(|| format!("invalid time zone '{}'", options.timezone))?;
        let api = ApiClient::new(
            &options.server_url,
            &options.username,
            &options.password,
            options.verbose_log.as_deref(),
        )?;
        let initial_client_id = if options.initial_client_id.trim().is_empty() {
            GLOBAL_CLIENT.to_string()
        } else {
            options.initial_client_id
        };
        let initial_alias = options.initial_alias;
        let mut app = Self {
            api,
            timezone,
            view: View::Overview,
            breakdown_kind: BreakdownKind::Clients,
            analytics_kind: AnalyticsKind::Calendar,
            calendar_offset_weeks: 0,
            breakdown_offset: 0,
            client_ids: vec![GLOBAL_CLIENT.to_string()],
            client_index: 0,
            aliases: vec![String::new()],
            alias_index: 0,
            overview: None,
            analytics: None,
            records: Vec::new(),
            records_total: 0,
            records_page: 1,
            page_size: i64::from(options.page_size),
            records_state: TableState::default(),
            active: Vec::new(),
            active_state: TableState::default(),
            tokens: Vec::new(),
            tokens_state: TableState::default(),
            overlay: None,
            notice: None,
            legacy_records_api: false,
            should_quit: false,
            last_refresh: Instant::now(),
        };
        app.refresh_client_ids(Some(&initial_client_id))?;
        app.refresh_aliases(Some(&initial_alias))?;
        app.refresh_all()?;
        Ok(app)
    }

    fn current_client(&self) -> &str {
        self.client_ids
            .get(self.client_index)
            .map(String::as_str)
            .unwrap_or(GLOBAL_CLIENT)
    }

    fn current_client_label(&self) -> &str {
        if self.current_client() == GLOBAL_CLIENT {
            "all clients"
        } else {
            self.current_client()
        }
    }

    fn current_alias(&self) -> &str {
        self.aliases
            .get(self.alias_index)
            .map(String::as_str)
            .unwrap_or_default()
    }

    fn current_alias_label(&self) -> &str {
        if self.current_alias().is_empty() {
            "all aliases"
        } else {
            self.current_alias()
        }
    }

    fn refresh_client_ids(&mut self, preferred: Option<&str>) -> Result<()> {
        let existing = preferred
            .map(str::to_owned)
            .unwrap_or_else(|| self.current_client().to_string());
        let mut clients = self.api.clients()?;
        clients.extend(self.api.tokens()?.into_iter().map(|token| token.client_id));
        clients.sort();
        clients.dedup();
        self.client_ids = vec![GLOBAL_CLIENT.to_string()];
        self.client_ids.extend(clients);
        if existing != GLOBAL_CLIENT && !self.client_ids.contains(&existing) {
            self.client_ids.push(existing.clone());
            self.client_ids[1..].sort();
        }
        self.client_index = self
            .client_ids
            .iter()
            .position(|client| client == &existing)
            .unwrap_or(0);
        Ok(())
    }

    fn refresh_aliases(&mut self, preferred: Option<&str>) -> Result<()> {
        let existing = preferred
            .map(str::to_owned)
            .unwrap_or_else(|| self.current_alias().to_string());
        let mut aliases = self.api.aliases()?;
        aliases.retain(|alias| !alias.is_empty());
        aliases.sort();
        aliases.dedup();
        self.aliases = vec![String::new()];
        self.aliases.extend(aliases);
        if !existing.is_empty() && !self.aliases.contains(&existing) {
            self.aliases.push(existing.clone());
            self.aliases[1..].sort();
        }
        self.alias_index = self
            .aliases
            .iter()
            .position(|alias| alias == &existing)
            .unwrap_or(0);
        Ok(())
    }

    fn refresh_all(&mut self) -> Result<()> {
        self.refresh_client_ids(None)?;
        self.refresh_aliases(None)?;
        let client_id = self.current_client().to_string();
        let alias = self.current_alias().to_string();
        let old_record_selection = self.records_state.selected().unwrap_or(0);
        let old_active_selection = self.active_state.selected().unwrap_or(0);
        let old_token_selection = self.tokens_state.selected().unwrap_or(0);

        let overview = self.api.overview(&client_id, &alias, self.timezone)?;
        let analytics = self.api.analytics(&client_id, &alias, self.timezone)?;
        let mut record_page =
            self.api
                .records(&client_id, &alias, self.records_page, self.page_size)?;
        let pages = total_pages(record_page.total, self.page_size);
        if self.records_page > pages {
            self.records_page = pages;
            record_page =
                self.api
                    .records(&client_id, &alias, self.records_page, self.page_size)?;
        }
        let missing_record_status = record_page
            .records
            .iter()
            .any(|record| record.status.is_none());
        let mut active = self.api.active_sessions()?;
        let mut tokens = self.api.tokens()?;
        if client_id != GLOBAL_CLIENT {
            active.retain(|record| record.client_id == client_id);
            tokens.retain(|token| token.client_id == client_id);
        }
        if !alias.is_empty() {
            active.retain(|record| record.alias.as_deref() == Some(alias.as_str()));
        }

        self.overview = Some(overview);
        self.analytics = Some(analytics);
        self.records = record_page.records;
        self.records_total = record_page.total;
        self.active = active;
        self.tokens = tokens;
        if missing_record_status && !self.legacy_records_api {
            self.legacy_records_api = true;
            self.set_warning(
                "Compatibility warning: this server omits record status; values are inferred locally. Upgrade tspan-server.",
            );
        }
        select_clamped(
            &mut self.records_state,
            old_record_selection,
            self.records.len(),
        );
        select_clamped(
            &mut self.active_state,
            old_active_selection,
            self.active.len(),
        );
        select_clamped(
            &mut self.tokens_state,
            old_token_selection,
            self.tokens.len(),
        );
        self.last_refresh = Instant::now();
        Ok(())
    }

    fn cycle_client(&mut self, forward: bool) {
        if self.client_ids.len() <= 1 {
            return;
        }
        if forward {
            self.client_index = (self.client_index + 1) % self.client_ids.len();
        } else {
            self.client_index =
                (self.client_index + self.client_ids.len() - 1) % self.client_ids.len();
        }
        self.records_page = 1;
        self.breakdown_offset = 0;
        self.calendar_offset_weeks = 0;
        if let Err(error) = self.refresh_all() {
            self.set_error(error);
        } else {
            self.set_notice(format!(
                "Showing {} · {}",
                self.current_client_label(),
                self.current_alias_label()
            ));
        }
    }

    fn cycle_alias(&mut self, forward: bool) {
        if self.aliases.len() <= 1 {
            return;
        }
        if forward {
            self.alias_index = (self.alias_index + 1) % self.aliases.len();
        } else {
            self.alias_index = (self.alias_index + self.aliases.len() - 1) % self.aliases.len();
        }
        self.records_page = 1;
        self.breakdown_offset = 0;
        self.calendar_offset_weeks = 0;
        if let Err(error) = self.refresh_all() {
            self.set_error(error);
        } else {
            self.set_notice(format!(
                "Showing {} · {}",
                self.current_client_label(),
                self.current_alias_label()
            ));
        }
    }

    fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: false,
            is_warning: false,
        });
    }

    fn set_warning(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: false,
            is_warning: true,
        });
    }

    fn set_error(&mut self, error: impl std::fmt::Display) {
        self.notice = Some(Notice {
            text: error.to_string(),
            is_error: true,
            is_warning: false,
        });
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.overlay.is_some() {
            self.handle_overlay_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Tab => self.view = self.view.next(),
            KeyCode::BackTab => self.view = self.view.previous(),
            KeyCode::Char('1') => self.view = View::Overview,
            KeyCode::Char('2') => self.view = View::Breakdown,
            KeyCode::Char('3') => self.view = View::Records,
            KeyCode::Char('4') => self.view = View::Active,
            KeyCode::Char('5') => self.view = View::Tokens,
            KeyCode::Char('6') => self.view = View::Analytics,
            KeyCode::Char('r') => match self.refresh_all() {
                Ok(()) => self.set_notice("Data refreshed"),
                Err(error) => self.set_error(error),
            },
            KeyCode::Char(']') => self.cycle_client(true),
            KeyCode::Char('[') => self.cycle_client(false),
            KeyCode::Char('}') => self.cycle_alias(true),
            KeyCode::Char('{') => self.cycle_alias(false),
            _ => self.handle_view_key(key.code),
        }
    }

    fn handle_view_key(&mut self, code: KeyCode) {
        match self.view {
            View::Overview => {}
            View::Analytics => match code {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.analytics_kind = self.analytics_kind.previous();
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.analytics_kind = self.analytics_kind.next();
                }
                KeyCode::Down | KeyCode::PageDown | KeyCode::Char('j')
                    if self.analytics_kind == AnalyticsKind::Calendar =>
                {
                    self.calendar_offset_weeks = self
                        .calendar_offset_weeks
                        .saturating_add(26)
                        .min(self.calendar_history_weeks());
                }
                KeyCode::Up | KeyCode::PageUp | KeyCode::Char('k')
                    if self.analytics_kind == AnalyticsKind::Calendar =>
                {
                    self.calendar_offset_weeks = self.calendar_offset_weeks.saturating_sub(26);
                }
                KeyCode::Home | KeyCode::Char('g')
                    if self.analytics_kind == AnalyticsKind::Calendar =>
                {
                    self.calendar_offset_weeks = 0;
                }
                _ => {}
            },
            View::Breakdown => match code {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.breakdown_kind = self.breakdown_kind.previous();
                    self.breakdown_offset = 0;
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.breakdown_kind = self.breakdown_kind.next();
                    self.breakdown_offset = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.breakdown_offset = self
                        .breakdown_offset
                        .saturating_add(1)
                        .min(self.breakdown_len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.breakdown_offset = self.breakdown_offset.saturating_sub(1);
                }
                KeyCode::Home | KeyCode::Char('g') => self.breakdown_offset = 0,
                _ => {}
            },
            View::Records => match code {
                KeyCode::Down | KeyCode::Char('j') => {
                    select_next(&mut self.records_state, self.records.len())
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    select_previous(&mut self.records_state, self.records.len())
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    select_first(&mut self.records_state, self.records.len())
                }
                KeyCode::End | KeyCode::Char('G') => {
                    select_last(&mut self.records_state, self.records.len())
                }
                KeyCode::Right | KeyCode::PageDown | KeyCode::Char('n') => {
                    self.change_record_page(1)
                }
                KeyCode::Left | KeyCode::PageUp | KeyCode::Char('p') => self.change_record_page(-1),
                KeyCode::Char('d') | KeyCode::Delete => {
                    if let Some(record) = self.selected_record() {
                        self.overlay = Some(Overlay::Confirm(AdminAction::DeleteRecord(record.id)));
                    }
                }
                _ => {}
            },
            View::Active => match code {
                KeyCode::Down | KeyCode::Char('j') => {
                    select_next(&mut self.active_state, self.active.len())
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    select_previous(&mut self.active_state, self.active.len())
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    select_first(&mut self.active_state, self.active.len())
                }
                KeyCode::End | KeyCode::Char('G') => {
                    select_last(&mut self.active_state, self.active.len())
                }
                KeyCode::Char('e') => {
                    if let Some(record) = self.selected_active() {
                        self.overlay = Some(Overlay::Confirm(AdminAction::EndSession(record.id)));
                    }
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    if let Some(record) = self.selected_active() {
                        self.overlay =
                            Some(Overlay::Confirm(AdminAction::DiscardSession(record.id)));
                    }
                }
                _ => {}
            },
            View::Tokens => match code {
                KeyCode::Down | KeyCode::Char('j') => {
                    select_next(&mut self.tokens_state, self.tokens.len())
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    select_previous(&mut self.tokens_state, self.tokens.len())
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    select_first(&mut self.tokens_state, self.tokens.len())
                }
                KeyCode::End | KeyCode::Char('G') => {
                    select_last(&mut self.tokens_state, self.tokens.len())
                }
                KeyCode::Char('n') => {
                    let default_client = if self.current_client() == GLOBAL_CLIENT {
                        "default"
                    } else {
                        self.current_client()
                    };
                    self.overlay = Some(Overlay::TokenForm {
                        field: TokenField::ClientId,
                        client_id: default_client.to_string(),
                        description: String::new(),
                    });
                }
                KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete => {
                    if let Some(token) = self.selected_token() {
                        self.overlay = Some(Overlay::Confirm(AdminAction::RevokeToken(
                            token.token.clone(),
                        )));
                    }
                }
                _ => {}
            },
        }
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) {
        match self.overlay.take() {
            Some(Overlay::Help) => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter) {
                    self.overlay = Some(Overlay::Help);
                }
            }
            Some(Overlay::Confirm(action)) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.perform_action(action),
                KeyCode::Esc | KeyCode::Char('n') => self.set_notice("Action cancelled"),
                _ => self.overlay = Some(Overlay::Confirm(action)),
            },
            Some(Overlay::TokenCreated(token)) => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    self.overlay = Some(Overlay::TokenCreated(token));
                }
            }
            Some(Overlay::TokenForm {
                mut field,
                mut client_id,
                mut description,
            }) => {
                let mut submit = false;
                match key.code {
                    KeyCode::Esc => {
                        self.set_notice("Token generation cancelled");
                        return;
                    }
                    KeyCode::Tab => {
                        field = match field {
                            TokenField::ClientId => TokenField::Description,
                            TokenField::Description => TokenField::ClientId,
                        };
                    }
                    KeyCode::BackTab => {
                        field = match field {
                            TokenField::ClientId => TokenField::Description,
                            TokenField::Description => TokenField::ClientId,
                        };
                    }
                    KeyCode::Enter if field == TokenField::ClientId => {
                        field = TokenField::Description;
                    }
                    KeyCode::Enter => submit = true,
                    KeyCode::Backspace => match field {
                        TokenField::ClientId => {
                            client_id.pop();
                        }
                        TokenField::Description => {
                            description.pop();
                        }
                    },
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        match field {
                            TokenField::ClientId => client_id.clear(),
                            TokenField::Description => description.clear(),
                        }
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        match field {
                            TokenField::ClientId => client_id.push(character),
                            TokenField::Description => description.push(character),
                        }
                    }
                    _ => {}
                }
                if submit {
                    self.create_token(client_id, description);
                } else {
                    self.overlay = Some(Overlay::TokenForm {
                        field,
                        client_id,
                        description,
                    });
                }
            }
            None => {}
        }
    }

    fn change_record_page(&mut self, delta: i64) {
        let pages = total_pages(self.records_total, self.page_size);
        let next = (self.records_page + delta).clamp(1, pages);
        if next == self.records_page {
            return;
        }
        self.records_page = next;
        self.records_state.select(None);
        if let Err(error) = self.refresh_all() {
            self.set_error(error);
        }
    }

    fn breakdown_len(&self) -> usize {
        let Some(data) = self.overview.as_ref() else {
            return 0;
        };
        match self.breakdown_kind {
            BreakdownKind::Clients => data.by_client.len(),
            BreakdownKind::Aliases => data.by_alias.len(),
            BreakdownKind::Commands => data.by_command.len(),
            BreakdownKind::Distribution => data.distribution.buckets.len(),
        }
    }

    fn calendar_history_weeks(&self) -> usize {
        let today = Utc::now().with_timezone(&self.timezone).date_naive();
        self.analytics
            .as_ref()
            .and_then(|analytics| {
                analytics
                    .daily
                    .iter()
                    .filter_map(|(day, _)| NaiveDate::parse_from_str(day, "%Y-%m-%d").ok())
                    .min()
            })
            .map(|earliest| today.signed_duration_since(earliest).num_weeks().max(0) as usize)
            .unwrap_or(0)
    }

    fn selected_record(&self) -> Option<&RecordPageItem> {
        self.records_state
            .selected()
            .and_then(|index| self.records.get(index))
    }

    fn selected_active(&self) -> Option<&OrphanedSession> {
        self.active_state
            .selected()
            .and_then(|index| self.active.get(index))
    }

    fn selected_token(&self) -> Option<&ApiToken> {
        self.tokens_state
            .selected()
            .and_then(|index| self.tokens.get(index))
    }

    fn perform_action(&mut self, action: AdminAction) {
        let result: Result<String> = (|| {
            let message = match action {
                AdminAction::DeleteRecord(id) => {
                    if self.api.delete_record(id)? {
                        format!("Record #{id} deleted")
                    } else {
                        format!("Record #{id} no longer exists")
                    }
                }
                AdminAction::EndSession(id) => match self.api.end_session(id)? {
                    Some(duration) => {
                        format!("Session #{id} ended ({})", human_readable_time(duration))
                    }
                    None => format!("Session #{id} is no longer active"),
                },
                AdminAction::DiscardSession(id) => {
                    if self.api.discard_session(id)? {
                        format!("Session #{id} discarded")
                    } else {
                        format!("Session #{id} is no longer active")
                    }
                }
                AdminAction::RevokeToken(token) => {
                    if self.api.revoke_token(&token)? {
                        format!("Token {} revoked", redact_token(&token))
                    } else {
                        "Token no longer exists".to_string()
                    }
                }
            };
            Ok(message)
        })();
        match result {
            Ok(message) => match self.refresh_all() {
                Ok(()) => self.set_notice(message),
                Err(error) => self.set_error(format!("{message}; refresh failed: {error}")),
            },
            Err(error) => self.set_error(error),
        }
    }

    fn create_token(&mut self, client_id: String, description: String) {
        let client_id = client_id.trim();
        if client_id.is_empty() {
            self.set_error("Client ID cannot be empty");
            self.overlay = Some(Overlay::TokenForm {
                field: TokenField::ClientId,
                client_id: String::new(),
                description,
            });
            return;
        }
        let description = description.trim();
        let result = self
            .api
            .create_token(client_id, (!description.is_empty()).then_some(description));
        match result {
            Ok(token) => match self.refresh_all() {
                Ok(()) => self.overlay = Some(Overlay::TokenCreated(token)),
                Err(error) => self.set_error(format!("Token created, but refresh failed: {error}")),
            },
            Err(error) => self.set_error(error),
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 60 || area.height < 16 {
            frame.render_widget(
                Paragraph::new(format!(
                    "Terminal is too small ({}x{}).\nResize to at least 60x16.\n\nPress q to quit.",
                    area.width, area.height
                ))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" TSPAN Admin ")
                        .border_style(Style::default().fg(Color::Yellow)),
                ),
                area,
            );
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(2),
            ])
            .split(area);
        self.draw_header(frame, chunks[0]);
        match self.view {
            View::Overview => self.draw_overview(frame, chunks[1]),
            View::Breakdown => self.draw_breakdown(frame, chunks[1]),
            View::Records => self.draw_records(frame, chunks[1]),
            View::Active => self.draw_active(frame, chunks[1]),
            View::Tokens => self.draw_tokens(frame, chunks[1]),
            View::Analytics => self.draw_analytics(frame, chunks[1]),
        }
        self.draw_footer(frame, chunks[2]);
        self.draw_overlay(frame, area);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let titles = View::ALL
            .iter()
            .enumerate()
            .map(|(index, view)| Line::from(format!(" {} {} ", index + 1, view.title())))
            .collect::<Vec<_>>();
        let compatibility = if self.legacy_records_api {
            " · LEGACY API"
        } else {
            ""
        };
        let title = format!(
            " TSPAN Admin · {} · {} · {} · {}{} ",
            self.api.label(),
            self.current_client_label(),
            self.current_alias_label(),
            self.timezone,
            compatibility
        );
        let tabs = Tabs::new(titles)
            .select(self.view.index())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(if self.legacy_records_api {
                        Color::Yellow
                    } else {
                        Color::DarkGray
                    })),
            )
            .style(Style::default().fg(Color::Gray))
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(Span::styled("│", Style::default().fg(Color::DarkGray)));
        frame.render_widget(tabs, area);
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let help = match self.view {
            View::Overview => "[/] client  {/} alias  r refresh  Tab view  ? help  q quit",
            View::Breakdown => "←/→ category  ↑/↓ scroll  [/] client  {/} alias  r refresh  ? help",
            View::Records => {
                "↑/↓ select  ←/→ page  d delete  [/] client  {/} alias  r refresh  ? help"
            }
            View::Active => {
                "↑/↓ select  e end  d discard  [/] client  {/} alias  r refresh  ? help"
            }
            View::Tokens => "↑/↓ select  n new  d revoke  [/] client  {/} alias  r refresh  ? help",
            View::Analytics => "←/→ graph  j/k history  [/] client  {/} alias  r refresh  ? help",
        };
        let notice = self.notice.as_ref();
        let notice_style = match notice {
            Some(notice) if notice.is_error => Style::default().fg(Color::Red),
            Some(notice) if notice.is_warning => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Green),
        };
        let lines = vec![
            Line::from(Span::styled(help, Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled(
                notice.map(|notice| notice.text.as_str()).unwrap_or(""),
                notice_style,
            )),
        ];
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn draw_overview(&self, frame: &mut Frame, area: Rect) {
        let Some(data) = self.overview.as_ref() else {
            frame.render_widget(Paragraph::new("No statistics loaded"), area);
            return;
        };
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(3),
                Constraint::Min(5),
            ])
            .split(area);
        let cards = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ])
            .split(rows[0]);
        draw_card(
            frame,
            cards[0],
            "Tracked time",
            &data.stats.total.total_seconds_hr,
            Color::Cyan,
        );
        draw_card(
            frame,
            cards[1],
            "Sessions",
            &data.stats.total.total_times.to_string(),
            Color::Green,
        );
        draw_card(
            frame,
            cards[2],
            "Active days",
            &format!(
                "{} ({:.1}%)",
                data.stats.total.active_days, data.stats.total.total_day_ratio
            ),
            Color::Yellow,
        );
        draw_card(
            frame,
            cards[3],
            "Current streak",
            &format!("{} days", data.streaks.current_streak),
            Color::Magenta,
        );

        let detail = format!(
            " Since {}  ·  Mean {}  ·  Median {}  ·  Last active {} ",
            data.stats.total.from_date,
            data.stats.total.mean_usage_hr,
            data.distribution.median_seconds_hr,
            data.streaks.last_active_time_hr,
        );
        frame.render_widget(
            Paragraph::new(detail)
                .style(Style::default().fg(Color::Gray))
                .block(Block::default().borders(Borders::ALL).title(" Details ")),
            rows[1],
        );

        let lower = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[2]);
        let recent_rows = data.stats.past_n.iter().map(|period| {
            Row::new(vec![
                Cell::from(period.name.clone()),
                Cell::from(human_readable_time(period.seconds)),
                Cell::from(period.times.to_string()),
                Cell::from(format!("{:.1}%", period.day_ratio)),
            ])
        });
        let recent = Table::new(
            recent_rows,
            [
                Constraint::Percentage(32),
                Constraint::Percentage(32),
                Constraint::Percentage(16),
                Constraint::Percentage(20),
            ],
        )
        .header(table_header(["Period", "Time", "Sessions", "Active days"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Recent activity "),
        )
        .column_spacing(1);
        frame.render_widget(recent, lower[0]);

        let command_rows = data.by_command.iter().take(12).map(|command| {
            Row::new(vec![
                Cell::from(command.command.clone()),
                Cell::from(command.total_seconds_hr.clone()),
                Cell::from(command.total_times.to_string()),
            ])
        });
        let commands = Table::new(
            command_rows,
            [
                Constraint::Min(18),
                Constraint::Length(18),
                Constraint::Length(10),
            ],
        )
        .header(table_header(["Top command", "Time", "Sessions"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Top commands "),
        )
        .column_spacing(1);
        frame.render_widget(commands, lower[1]);
    }

    fn draw_breakdown(&self, frame: &mut Frame, area: Rect) {
        let Some(data) = self.overview.as_ref() else {
            frame.render_widget(Paragraph::new("No statistics loaded"), area);
            return;
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(5)])
            .split(area);
        let tabs = Tabs::new(
            BreakdownKind::ALL
                .iter()
                .map(|kind| Line::from(format!(" {} ", kind.title())))
                .collect::<Vec<_>>(),
        )
        .select(self.breakdown_kind.index())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Statistics breakdown "),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");
        frame.render_widget(tabs, chunks[0]);

        match self.breakdown_kind {
            BreakdownKind::Clients => {
                let rows = data
                    .by_client
                    .iter()
                    .skip(self.breakdown_offset)
                    .map(|item| {
                        Row::new(vec![
                            Cell::from(item.client_id.clone()),
                            Cell::from(item.total_seconds_hr.clone()),
                            Cell::from(item.total_times.to_string()),
                            Cell::from(item.mean_seconds_hr.clone()),
                        ])
                    });
                frame.render_widget(breakdown_table(rows, " By client ", "Client"), chunks[1]);
            }
            BreakdownKind::Aliases => {
                let rows = data
                    .by_alias
                    .iter()
                    .skip(self.breakdown_offset)
                    .map(|item| {
                        Row::new(vec![
                            Cell::from(item.alias.clone()),
                            Cell::from(item.total_seconds_hr.clone()),
                            Cell::from(item.total_times.to_string()),
                            Cell::from(item.mean_seconds_hr.clone()),
                        ])
                    });
                frame.render_widget(breakdown_table(rows, " By alias ", "Alias"), chunks[1]);
            }
            BreakdownKind::Commands => {
                let rows = data
                    .by_command
                    .iter()
                    .skip(self.breakdown_offset)
                    .map(|item| {
                        Row::new(vec![
                            Cell::from(item.command.clone()),
                            Cell::from(item.total_seconds_hr.clone()),
                            Cell::from(item.total_times.to_string()),
                            Cell::from(item.mean_seconds_hr.clone()),
                        ])
                    });
                frame.render_widget(breakdown_table(rows, " By command ", "Command"), chunks[1]);
            }
            BreakdownKind::Distribution => {
                let rows = data
                    .distribution
                    .buckets
                    .iter()
                    .skip(self.breakdown_offset)
                    .map(|bucket| {
                        Row::new(vec![
                            Cell::from(bucket.label.clone()),
                            Cell::from(bucket.count.to_string()),
                            Cell::from(format!("{:.1}%", bucket.pct)),
                        ])
                    });
                let summary = format!(
                    " Durations · min {} · median {} · mean {} · max {} ",
                    data.distribution.min_seconds_hr,
                    data.distribution.median_seconds_hr,
                    data.distribution.mean_seconds_hr,
                    data.distribution.max_seconds_hr,
                );
                let table = Table::new(
                    rows,
                    [
                        Constraint::Percentage(50),
                        Constraint::Percentage(25),
                        Constraint::Percentage(25),
                    ],
                )
                .header(table_header(["Duration", "Sessions", "Share"]))
                .block(Block::default().borders(Borders::ALL).title(summary))
                .column_spacing(2);
                frame.render_widget(table, chunks[1]);
            }
        }
    }

    fn draw_analytics(&self, frame: &mut Frame, area: Rect) {
        let Some(data) = self.analytics.as_ref() else {
            frame.render_widget(Paragraph::new("No analytics loaded"), area);
            return;
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(5)])
            .split(area);
        let tabs = Tabs::new(
            AnalyticsKind::ALL
                .iter()
                .map(|kind| Line::from(format!(" {} ", kind.title())))
                .collect::<Vec<_>>(),
        )
        .select(self.analytics_kind.index())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Dashboard graphs "),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");
        frame.render_widget(tabs, chunks[0]);

        match self.analytics_kind {
            AnalyticsKind::Calendar => self.draw_activity_calendar(frame, chunks[1], data),
            AnalyticsKind::Monthly => self.draw_monthly_trend(frame, chunks[1], data),
            AnalyticsKind::Hourly => self.draw_hourly_heatmap(frame, chunks[1], data),
            AnalyticsKind::Patterns => self.draw_activity_patterns(frame, chunks[1], data),
        }
    }

    fn draw_activity_calendar(&self, frame: &mut Frame, area: Rect, data: &AnalyticsData) {
        let today = Utc::now().with_timezone(&self.timezone).date_naive();
        let cell_width = 2_u16;
        let weeks = usize::from((area.width.saturating_sub(8) / cell_width).clamp(4, 53));
        let this_monday = today
            - ChronoDuration::days(i64::from(today.weekday().num_days_from_monday()))
            - ChronoDuration::weeks(self.calendar_offset_weeks as i64);
        let start = this_monday - ChronoDuration::days(((weeks - 1) * 7) as i64);
        let end = start + ChronoDuration::days((weeks * 7 - 1) as i64);
        let values = data
            .daily
            .iter()
            .map(|(day, seconds)| (day.as_str(), *seconds))
            .collect::<HashMap<_, _>>();
        let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let mut lines = Vec::with_capacity(7);
        for (weekday, name) in day_names.iter().enumerate() {
            let mut spans = vec![Span::styled(
                format!("{name} "),
                Style::default().fg(Color::Gray),
            )];
            for week in 0..weeks {
                let date = start + ChronoDuration::days((week * 7 + weekday) as i64);
                if date > today {
                    spans.push(Span::raw("  "));
                    continue;
                }
                let key = date.format("%Y-%m-%d").to_string();
                let seconds = values.get(key.as_str()).copied().unwrap_or(0);
                spans.push(Span::styled(
                    "██",
                    Style::default().fg(calendar_activity_color(seconds)),
                ));
            }
            lines.push(Line::from(spans));
        }
        let title = format!(
            " Activity · {} → {} · 0 | <30m | <60m | ≥60m ",
            start.format("%Y-%m-%d"),
            end.min(today).format("%Y-%m-%d")
        );
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
            area,
        );
    }

    fn draw_monthly_trend(&self, frame: &mut Frame, area: Rect, data: &AnalyticsData) {
        if data.monthly.is_empty() {
            frame.render_widget(
                Paragraph::new("No monthly activity data")
                    .alignment(Alignment::Center)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Monthly trend "),
                    ),
                area,
            );
            return;
        }
        let capacity = usize::from(area.width.saturating_sub(2)).max(1);
        let visible = &data.monthly[data.monthly.len().saturating_sub(capacity)..];
        let durations = visible
            .iter()
            .map(|point| point.total_seconds.max(0) as u64)
            .collect::<Vec<_>>();
        let sessions = visible
            .iter()
            .map(|point| point.total_times.max(0) as u64)
            .collect::<Vec<_>>();
        let first = &visible[0];
        let latest = &visible[visible.len() - 1];
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        frame.render_widget(
            Sparkline::default()
                .data(&durations)
                .style(Style::default().fg(Color::Cyan))
                .block(Block::default().borders(Borders::ALL).title(format!(
                    " Duration · {} → {} · latest {} ",
                    first.year_month, latest.year_month, latest.total_seconds_hr
                ))),
            rows[0],
        );
        frame.render_widget(
            Sparkline::default()
                .data(&sessions)
                .style(Style::default().fg(Color::Green))
                .block(Block::default().borders(Borders::ALL).title(format!(
                    " Sessions · {} → {} · latest {} ",
                    first.year_month, latest.year_month, latest.total_times
                ))),
            rows[1],
        );
    }

    fn draw_hourly_heatmap(&self, frame: &mut Frame, area: Rect, data: &AnalyticsData) {
        let cell_width = if area.width >= 56 { 2 } else { 1 };
        let mut lines = Vec::with_capacity(8);
        if cell_width == 2 {
            let mut header = vec![Span::raw("    ")];
            for hour in 0..24 {
                header.push(Span::styled(
                    if hour % 4 == 0 {
                        format!("{hour:02}")
                    } else {
                        "  ".to_string()
                    },
                    Style::default().fg(Color::Gray),
                ));
            }
            lines.push(Line::from(header));
        }
        let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        for (day, name) in day_names.iter().enumerate() {
            let mut spans = vec![Span::styled(
                format!("{name} "),
                Style::default().fg(Color::Gray),
            )];
            for hour in 0..24 {
                let seconds = data
                    .hourly
                    .grid
                    .get(day)
                    .and_then(|row| row.get(hour))
                    .copied()
                    .unwrap_or(0);
                spans.push(Span::styled(
                    if cell_width == 2 { "██" } else { "█" },
                    Style::default().fg(relative_heat_color(seconds, data.hourly.max_seconds)),
                ));
            }
            lines.push(Line::from(spans));
        }
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Hourly heatmap · columns 00–23 · dark low → red high "),
            ),
            area,
        );
    }

    fn draw_activity_patterns(&self, frame: &mut Frame, area: Rect, data: &AnalyticsData) {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        let weekday = &data.weekday_weekend;
        let metric_width = usize::from(panes[0].width.saturating_sub(23)).max(1);
        let mut weekday_lines = Vec::with_capacity(6);
        let total_max = weekday
            .weekday_total_seconds
            .max(weekday.weekend_total_seconds);
        weekday_lines.push(metric_bar_line(
            "Time WD",
            weekday.weekday_total_seconds,
            total_max,
            &weekday.weekday_total_hr,
            Color::Cyan,
            metric_width,
        ));
        weekday_lines.push(metric_bar_line(
            "Time WE",
            weekday.weekend_total_seconds,
            total_max,
            &weekday.weekend_total_hr,
            Color::Green,
            metric_width,
        ));
        let times_max = weekday.weekday_times.max(weekday.weekend_times);
        weekday_lines.push(metric_bar_line(
            "Sess WD",
            weekday.weekday_times,
            times_max,
            &weekday.weekday_times.to_string(),
            Color::Cyan,
            metric_width,
        ));
        weekday_lines.push(metric_bar_line(
            "Sess WE",
            weekday.weekend_times,
            times_max,
            &weekday.weekend_times.to_string(),
            Color::Green,
            metric_width,
        ));
        let mean_max = weekday
            .weekday_mean_seconds
            .max(weekday.weekend_mean_seconds);
        weekday_lines.push(metric_bar_line(
            "Mean WD",
            weekday.weekday_mean_seconds,
            mean_max,
            &weekday.weekday_mean_hr,
            Color::Cyan,
            metric_width,
        ));
        weekday_lines.push(metric_bar_line(
            "Mean WE",
            weekday.weekend_mean_seconds,
            mean_max,
            &weekday.weekend_mean_hr,
            Color::Green,
            metric_width,
        ));
        frame.render_widget(
            Paragraph::new(weekday_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Weekday vs weekend "),
            ),
            panes[0],
        );

        let distribution = self.overview.as_ref().map(|value| &value.distribution);
        let buckets = distribution
            .map(|value| value.buckets.as_slice())
            .unwrap_or(&[]);
        let count_max = buckets.iter().map(|bucket| bucket.count).max().unwrap_or(0);
        let bucket_width = usize::from(panes[1].width.saturating_sub(21)).max(1);
        let bucket_lines = buckets.iter().map(|bucket| {
            metric_bar_line(
                &bucket.label,
                bucket.count,
                count_max,
                &format!("{} {:.0}%", bucket.count, bucket.pct),
                Color::Magenta,
                bucket_width,
            )
        });
        let title = distribution
            .map(|value| format!(" Distribution · median {} ", value.median_seconds_hr))
            .unwrap_or_else(|| " Session distribution ".to_string());
        frame.render_widget(
            Paragraph::new(bucket_lines.collect::<Vec<_>>())
                .block(Block::default().borders(Borders::ALL).title(title)),
            panes[1],
        );
    }

    fn draw_records(&mut self, frame: &mut Frame, area: Rect) {
        let rows = self.records.iter().map(|record| {
            Row::new(vec![
                Cell::from(record.id.to_string()),
                Cell::from(format_timestamp(record.start_time, self.timezone)),
                Cell::from(record.client_id.clone()),
                Cell::from(human_readable_time(
                    record.duration_seconds.unwrap_or_default(),
                )),
                Cell::from(record.alias.clone().unwrap_or_default()),
                Cell::from(record.command.clone().unwrap_or_default()),
                Cell::from(record.status_label().to_string()),
            ])
        });
        let pages = total_pages(self.records_total, self.page_size);
        let title = format!(
            " Records · page {}/{} · {} total ",
            self.records_page, pages, self.records_total
        );
        let table = Table::new(
            rows,
            [
                Constraint::Length(7),
                Constraint::Length(17),
                Constraint::Length(14),
                Constraint::Length(15),
                Constraint::Length(16),
                Constraint::Min(20),
                Constraint::Length(10),
            ],
        )
        .header(table_header([
            "ID", "Started", "Client", "Duration", "Alias", "Command", "Status",
        ]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ")
        .column_spacing(1);
        frame.render_stateful_widget(table, area, &mut self.records_state);
    }

    fn draw_active(&mut self, frame: &mut Frame, area: Rect) {
        let now = Utc::now().timestamp();
        let rows = self.active.iter().map(|record| {
            Row::new(vec![
                Cell::from(record.id.to_string()),
                Cell::from(format_timestamp(record.start_time, self.timezone)),
                Cell::from(record.client_id.clone()),
                Cell::from(human_readable_time(now.saturating_sub(record.start_time))),
                Cell::from(record.alias.clone().unwrap_or_default()),
                Cell::from(record.command.clone().unwrap_or_default()),
            ])
        });
        let title = format!(" Active sessions · {} ", self.active.len());
        let table = Table::new(
            rows,
            [
                Constraint::Length(7),
                Constraint::Length(17),
                Constraint::Length(14),
                Constraint::Length(15),
                Constraint::Length(18),
                Constraint::Min(20),
            ],
        )
        .header(table_header([
            "ID", "Started", "Client", "Elapsed", "Alias", "Command",
        ]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ")
        .column_spacing(1);
        frame.render_stateful_widget(table, area, &mut self.active_state);
    }

    fn draw_tokens(&mut self, frame: &mut Frame, area: Rect) {
        let rows = self.tokens.iter().map(|token| {
            Row::new(vec![
                Cell::from(token.token.clone()),
                Cell::from(token.client_id.clone()),
                Cell::from(token.description.clone().unwrap_or_default()),
                Cell::from(format_timestamp(token.created_at, self.timezone)),
            ])
        });
        let title = format!(" API tokens · {} ", self.tokens.len());
        let table = Table::new(
            rows,
            [
                Constraint::Min(39),
                Constraint::Length(18),
                Constraint::Percentage(35),
                Constraint::Length(17),
            ],
        )
        .header(table_header(["Token", "Client", "Description", "Created"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ")
        .column_spacing(1);
        frame.render_stateful_widget(table, area, &mut self.tokens_state);
    }

    fn draw_overlay(&self, frame: &mut Frame, area: Rect) {
        let Some(overlay) = self.overlay.as_ref() else {
            return;
        };
        match overlay {
            Overlay::Help => {
                let popup = centered_rect(82, 92, area);
                frame.render_widget(Clear, popup);
                let help = vec![
                    Line::from("Navigation"),
                    Line::from("  1–6 / Tab   switch views"),
                    Line::from("  ↑/↓ / j/k   select or scroll"),
                    Line::from("  [ / ]       change client filter"),
                    Line::from("  { / }       change alias filter"),
                    Line::from("  r           refresh (automatic every 10s)"),
                    Line::from("  q / Ctrl-C  quit"),
                    Line::from("View actions"),
                    Line::from("  Breakdown   ←/→ category"),
                    Line::from("  Records     ←/→ page · d delete"),
                    Line::from("  Active      e end · d discard"),
                    Line::from("  Tokens      n new · d revoke"),
                    Line::from("  Analytics   ←/→ chart"),
                    Line::from("              j/k calendar history"),
                    Line::from("Esc / Enter / ? closes this help."),
                ];
                frame.render_widget(
                    Paragraph::new(help).wrap(Wrap { trim: false }).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Keyboard help ")
                            .border_style(Style::default().fg(Color::Cyan)),
                    ),
                    popup,
                );
            }
            Overlay::Confirm(action) => {
                let popup = centered_rect(68, 50, area);
                frame.render_widget(Clear, popup);
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from(""),
                        Line::from(action.prompt()),
                        Line::from(""),
                        Line::from(vec![
                            Span::styled(" y / Enter ", Style::default().fg(Color::Red)),
                            Span::raw("confirm    "),
                            Span::styled(" n / Esc ", Style::default().fg(Color::Green)),
                            Span::raw("cancel"),
                        ]),
                    ])
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Confirm admin action ")
                            .border_style(Style::default().fg(Color::Red)),
                    ),
                    popup,
                );
            }
            Overlay::TokenForm {
                field,
                client_id,
                description,
            } => {
                let popup = centered_rect(74, 65, area);
                frame.render_widget(Clear, popup);
                let active = Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD);
                let inactive = Style::default().fg(Color::Gray);
                let lines = vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            " Client ID   ",
                            if *field == TokenField::ClientId {
                                active
                            } else {
                                inactive
                            },
                        ),
                        Span::raw(client_id),
                        Span::styled(
                            if *field == TokenField::ClientId {
                                "█"
                            } else {
                                ""
                            },
                            active,
                        ),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            " Description ",
                            if *field == TokenField::Description {
                                active
                            } else {
                                inactive
                            },
                        ),
                        Span::raw(description),
                        Span::styled(
                            if *field == TokenField::Description {
                                "█"
                            } else {
                                ""
                            },
                            active,
                        ),
                    ]),
                    Line::from(""),
                    Line::from(
                        "Tab changes field · Ctrl-U clears · Enter advances/creates · Esc cancels",
                    ),
                ];
                frame.render_widget(
                    Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Generate API token ")
                            .border_style(Style::default().fg(Color::Cyan)),
                    ),
                    popup,
                );
            }
            Overlay::TokenCreated(token) => {
                let popup = centered_rect(80, 50, area);
                frame.render_widget(Clear, popup);
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from(""),
                        Line::from("Token generated. Save it now:"),
                        Line::from(""),
                        Line::from(Span::styled(
                            token,
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::from(""),
                        Line::from("Press Enter or Esc to close."),
                    ])
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" API token created ")
                            .border_style(Style::default().fg(Color::Green)),
                    ),
                    popup,
                );
            }
        }
    }
}

fn calendar_activity_color(seconds: i64) -> Color {
    match seconds {
        value if value <= 0 => Color::DarkGray,
        1..=1_799 => Color::Green,
        1_800..=3_600 => Color::Yellow,
        _ => Color::Red,
    }
}

fn relative_heat_color(seconds: i64, max_seconds: i64) -> Color {
    if seconds <= 0 || max_seconds <= 0 {
        return Color::DarkGray;
    }
    let ratio = seconds as f64 / max_seconds as f64;
    if ratio < 0.33 {
        Color::Green
    } else if ratio < 0.66 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn metric_bar_line(
    label: &str,
    value: i64,
    max_value: i64,
    display: &str,
    color: Color,
    width: usize,
) -> Line<'static> {
    let filled = if value <= 0 || max_value <= 0 {
        0
    } else {
        (((value as f64 / max_value as f64) * width as f64).round() as usize).clamp(1, width)
    };
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(width - filled));
    Line::from(vec![
        Span::styled(format!("{label:<8} "), Style::default().fg(Color::Gray)),
        Span::styled(bar, Style::default().fg(color)),
        Span::raw(format!(" {display}")),
    ])
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("could not enable terminal raw mode")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(error).context("could not enter the alternate terminal screen");
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
    }
}

pub fn run(options: TuiOptions) -> Result<()> {
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "the TUI requires an interactive terminal"
    );
    let mut app = App::new(options)?;
    let guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("could not initialize terminal")?;

    let result = run_event_loop(&mut terminal, &mut app);
    drop(terminal);
    drop(guard);
    result
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    while !app.should_quit {
        terminal
            .draw(|frame| app.draw(frame))
            .context("could not draw terminal interface")?;
        if event::poll(Duration::from_millis(250)).context("could not poll terminal events")? {
            match event::read().context("could not read terminal event")? {
                Event::Key(key) => app.handle_key(key),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        if app.last_refresh.elapsed() >= AUTO_REFRESH_INTERVAL && app.overlay.is_none() {
            if let Err(error) = app.refresh_all() {
                app.set_error(error);
                app.last_refresh = Instant::now();
            }
        }
    }
    Ok(())
}

fn draw_card(frame: &mut Frame, area: Rect, title: &str, value: &str, color: Color) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            value,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
        ),
        area,
    );
}

fn table_header<const N: usize>(values: [&str; N]) -> Row<'static> {
    Row::new(
        values
            .into_iter()
            .map(|value| Cell::from(value.to_string()))
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(1)
}

fn breakdown_table<'a, I>(rows: I, title: &str, first_column: &str) -> Table<'a>
where
    I: IntoIterator<Item = Row<'a>>,
{
    Table::new(
        rows,
        [
            Constraint::Min(24),
            Constraint::Length(20),
            Constraint::Length(12),
            Constraint::Length(20),
        ],
    )
    .header(table_header([
        first_column,
        "Total time",
        "Sessions",
        "Mean time",
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title.to_string()),
    )
    .column_spacing(2)
}

fn format_timestamp(timestamp: i64, timezone: Tz) -> String {
    DateTime::from_timestamp(timestamp, 0)
        .map(|value| {
            value
                .with_timezone(&timezone)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "invalid timestamp".to_string())
}

fn total_pages(total: i64, page_size: i64) -> i64 {
    ((total.max(0) + page_size - 1) / page_size).max(1)
}

fn redact_token(token: &str) -> String {
    if token.chars().count() <= 14 {
        return token.to_string();
    }
    let prefix: String = token.chars().take(10).collect();
    let suffix: String = token
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

fn select_clamped(state: &mut TableState, preferred: usize, len: usize) {
    state.select(if len > 0 {
        Some(preferred.min(len - 1))
    } else {
        None
    });
}

fn select_first(state: &mut TableState, len: usize) {
    state.select((len > 0).then_some(0));
}

fn select_last(state: &mut TableState, len: usize) {
    state.select((len > 0).then_some(len.saturating_sub(1)));
}

fn select_next(state: &mut TableState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    state.select(Some(match state.selected() {
        Some(index) if index + 1 < len => index + 1,
        _ => 0,
    }));
}

fn select_previous(state: &mut TableState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    state.select(Some(match state.selected() {
        Some(0) | None => len - 1,
        Some(index) => index - 1,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread::{self, JoinHandle},
    };

    #[derive(Clone, Copy)]
    enum Fixture {
        Empty,
        Workstation,
        Legacy,
        Actions,
    }

    struct MockState {
        fixture: Fixture,
        requests: Mutex<Vec<String>>,
        record_deleted: AtomicBool,
        session_ended: AtomicBool,
        token_revoked: AtomicBool,
    }

    struct TestServer {
        address: SocketAddr,
        stop: Arc<AtomicBool>,
        state: Arc<MockState>,
        thread: Option<JoinHandle<()>>,
        url: String,
    }

    impl TestServer {
        fn new(fixture: Fixture) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let state = Arc::new(MockState {
                fixture,
                requests: Mutex::new(Vec::new()),
                record_deleted: AtomicBool::new(false),
                session_ended: AtomicBool::new(false),
                token_revoked: AtomicBool::new(false),
            });
            let server_state = state.clone();
            let thread_stop = stop.clone();
            let thread = thread::spawn(move || {
                for stream in listener.incoming() {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        serve_request(stream, &state);
                    }
                }
            });
            Self {
                address,
                stop,
                state: server_state,
                thread: Some(thread),
                url: format!("http://{address}"),
            }
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.address);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    fn serve_request(mut stream: TcpStream, state: &MockState) {
        let mut bytes = [0_u8; 16 * 1024];
        let Ok(length) = stream.read(&mut bytes) else {
            return;
        };
        let request = String::from_utf8_lossy(&bytes[..length]);
        let expected = base64::engine::general_purpose::STANDARD.encode("admin:secret");
        let authenticated = request
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&format!("Authorization: Basic {expected}")));
        let Some(first_line) = request.lines().next() else {
            return;
        };
        let mut parts = first_line.split_whitespace();
        let method = parts.next().unwrap_or_default();
        let path = parts.next().unwrap_or_default();
        state.requests.lock().unwrap().push(path.to_string());
        let (status, body) = if authenticated {
            mock_response(method, path, state)
        } else {
            (401, json!({ "error": "unauthorized" }).to_string())
        };
        let status_text = match status {
            200 => "OK",
            401 => "Unauthorized",
            404 => "Not Found",
            _ => "Error",
        };
        write!(
            stream,
            "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    fn mock_response(method: &str, path: &str, state: &MockState) -> (u16, String) {
        let route = path.split('?').next().unwrap_or(path);
        match (method, route) {
            ("GET", "/api/clients") => (200, clients_payload(state.fixture)),
            ("GET", "/api/aliases") => (200, aliases_payload(state.fixture)),
            ("GET", "/api/admin/tokens") => (200, tokens_payload(state)),
            ("GET", "/api/records") => (200, records_payload(state, path)),
            ("GET", "/api/sessions/orphaned") => (200, active_payload(state)),
            ("GET", "/api/stats") => (200, stats_payload(state.fixture)),
            ("GET", "/api/stats/streaks") => (200, streaks_payload()),
            ("GET", "/api/stats/session-distribution") => {
                (200, distribution_payload(state.fixture))
            }
            ("GET", "/api/daily-data") => (200, daily_payload(state.fixture)),
            ("GET", "/api/stats/monthly-trend") => (200, monthly_payload(state.fixture)),
            ("GET", "/api/stats/hourly-heatmap") => (200, hourly_payload(state.fixture)),
            ("GET", "/api/stats/weekday-weekend") => (200, weekday_weekend_payload(state.fixture)),
            ("GET", "/api/stats/by-client")
            | ("GET", "/api/stats/by-alias")
            | ("GET", "/api/stats/by-command") => (200, "[]".to_string()),
            ("DELETE", "/api/admin/records/1") => {
                state.record_deleted.store(true, Ordering::Relaxed);
                (200, "{}".to_string())
            }
            ("POST", "/api/sessions/2/end") => {
                state.session_ended.store(true, Ordering::Relaxed);
                (
                    200,
                    json!({ "session_id": 2, "duration_seconds": 60 }).to_string(),
                )
            }
            ("POST", "/api/sessions/2/discard") => {
                state.session_ended.store(true, Ordering::Relaxed);
                (200, "{}".to_string())
            }
            ("POST", "/api/admin/tokens") => (200, json!({ "token": "tspan_created" }).to_string()),
            ("DELETE", "/api/admin/tokens/tspan_revoke_me") => {
                state.token_revoked.store(true, Ordering::Relaxed);
                (200, "{}".to_string())
            }
            _ => (404, json!({ "error": "not found" }).to_string()),
        }
    }

    fn clients_payload(fixture: Fixture) -> String {
        match fixture {
            Fixture::Empty => json!([]),
            Fixture::Workstation => json!(["workstation", "other"]),
            Fixture::Legacy => json!(["network"]),
            Fixture::Actions => json!(["client"]),
        }
        .to_string()
    }

    fn aliases_payload(fixture: Fixture) -> String {
        match fixture {
            Fixture::Empty | Fixture::Actions => json!([]),
            Fixture::Workstation => json!(["development", "meetings"]),
            Fixture::Legacy => json!(["网络中断: 192.168.71.1"]),
        }
        .to_string()
    }

    fn tokens_payload(state: &MockState) -> String {
        let tokens = match state.fixture {
            Fixture::Empty => json!([]),
            Fixture::Workstation => json!([
                {
                    "token": "tspan_test",
                    "client_id": "workstation",
                    "description": "test",
                    "created_at": 1_700_000_000
                },
                {
                    "token": "tspan_other",
                    "client_id": "other",
                    "description": null,
                    "created_at": 1_700_000_000
                }
            ]),
            Fixture::Legacy => json!([]),
            Fixture::Actions if !state.token_revoked.load(Ordering::Relaxed) => json!([{
                "token": "tspan_revoke_me",
                "client_id": "client",
                "description": null,
                "created_at": 1_700_000_000
            }]),
            Fixture::Actions => json!([]),
        };
        tokens.to_string()
    }

    fn records_payload(state: &MockState, path: &str) -> String {
        let mut records = Vec::new();
        match state.fixture {
            Fixture::Workstation if !path.contains("alias=meetings") => records.push(json!({
                "id": 1,
                "client_id": "workstation",
                "alias": "development",
                "command": "cargo test",
                "start_time": 1_700_000_000,
                "end_time": 1_700_000_120,
                "duration_seconds": 120,
                "status": "completed"
            })),
            Fixture::Workstation => {}
            Fixture::Legacy => records.push(json!({
                "id": 1083,
                "client_id": "network",
                "alias": "网络中断: 192.168.71.1",
                "command": "ping 192.168.71.1",
                "start_time": 1_783_294_212_i64,
                "end_time": 1_783_297_948_i64,
                "duration_seconds": 3_736
            })),
            Fixture::Actions => {
                if !state.record_deleted.load(Ordering::Relaxed) {
                    records.push(json!({
                        "id": 1,
                        "client_id": "client",
                        "alias": null,
                        "command": "done",
                        "start_time": 100,
                        "end_time": 160,
                        "duration_seconds": 60,
                        "status": "completed"
                    }));
                }
                if state.session_ended.load(Ordering::Relaxed) {
                    records.push(json!({
                        "id": 2,
                        "client_id": "client",
                        "alias": null,
                        "command": "running",
                        "start_time": 200,
                        "end_time": 260,
                        "duration_seconds": 60,
                        "status": "completed"
                    }));
                }
            }
            Fixture::Empty => {}
        }
        json!({
            "total": records.len(),
            "page": 1,
            "per_page": 25,
            "total_pages": 1,
            "records": records
        })
        .to_string()
    }

    fn active_payload(state: &MockState) -> String {
        let active = match state.fixture {
            Fixture::Workstation => json!([
                {
                    "id": 2,
                    "client_id": "workstation",
                    "start_time": 1_700_000_200,
                    "running_seconds": 30,
                    "command": "vim",
                    "alias": "development",
                    "process_id": null
                },
                {
                    "id": 3,
                    "client_id": "other",
                    "start_time": 1_700_000_200,
                    "running_seconds": 30,
                    "command": "sleep",
                    "alias": null,
                    "process_id": null
                }
            ]),
            Fixture::Actions if !state.session_ended.load(Ordering::Relaxed) => json!([{
                "id": 2,
                "client_id": "client",
                "start_time": 200,
                "running_seconds": 30,
                "command": "running",
                "alias": null,
                "process_id": null
            }]),
            Fixture::Empty | Fixture::Legacy | Fixture::Actions => json!([]),
        };
        active.to_string()
    }

    fn stats_payload(fixture: Fixture) -> String {
        let total_times = i64::from(matches!(fixture, Fixture::Workstation | Fixture::Legacy));
        json!({
            "total": {
                "total_days": 1,
                "active_days": total_times,
                "total_seconds": total_times * 120,
                "total_times": total_times,
                "mean_usage": total_times * 120,
                "total_ratio": 0.0,
                "total_day_ratio": 0.0,
                "from_date": "2023-11-14",
                "total_duration_hr": "00 h 02 m 00 s",
                "total_seconds_hr": "00 h 02 m 00 s",
                "mean_usage_hr": "00 h 02 m 00 s"
            },
            "past_n": [],
            "interval": {
                "current_interval": 0,
                "current_interval_hr": "0 s",
                "max_interval": 0,
                "max_interval_hr": "0 s",
                "mean_interval": 0,
                "mean_interval_hr": "0 s"
            }
        })
        .to_string()
    }

    fn streaks_payload() -> String {
        json!({
            "current_streak": 0,
            "max_streak": 0,
            "last_active_date": "",
            "last_active_time_hr": "0 s"
        })
        .to_string()
    }

    fn distribution_payload(fixture: Fixture) -> String {
        if matches!(fixture, Fixture::Workstation) {
            json!({
                "max_seconds": 5_400,
                "min_seconds": 600,
                "median_seconds": 1_800,
                "mean_seconds": 2_400,
                "total_sessions": 6,
                "max_seconds_hr": "01 h 30 m 00 s",
                "min_seconds_hr": "10 m 00 s",
                "median_seconds_hr": "30 m 00 s",
                "mean_seconds_hr": "40 m 00 s",
                "buckets": [
                    { "label": "< 30m", "count": 2, "pct": 33.3 },
                    { "label": "30-60m", "count": 3, "pct": 50.0 },
                    { "label": "> 60m", "count": 1, "pct": 16.7 }
                ]
            })
            .to_string()
        } else {
            json!({
                "max_seconds": 0,
                "min_seconds": 0,
                "median_seconds": 0,
                "mean_seconds": 0,
                "total_sessions": 0,
                "max_seconds_hr": "0 s",
                "min_seconds_hr": "0 s",
                "median_seconds_hr": "0 s",
                "mean_seconds_hr": "0 s",
                "buckets": []
            })
            .to_string()
        }
    }

    fn daily_payload(fixture: Fixture) -> String {
        if matches!(fixture, Fixture::Workstation) {
            json!([
                ["2024-01-01", 600],
                ["2026-07-12", 900],
                ["2026-07-13", 2_700],
                ["2026-07-14", 5_400]
            ])
            .to_string()
        } else {
            "[]".to_string()
        }
    }

    fn monthly_payload(fixture: Fixture) -> String {
        if matches!(fixture, Fixture::Workstation) {
            json!([
                {
                    "year_month": "2026-06",
                    "total_seconds": 3_600,
                    "total_times": 4,
                    "total_seconds_hr": "01 h 00 s"
                },
                {
                    "year_month": "2026-07",
                    "total_seconds": 9_000,
                    "total_times": 7,
                    "total_seconds_hr": "02 h 30 m 00 s"
                }
            ])
            .to_string()
        } else {
            "[]".to_string()
        }
    }

    fn hourly_payload(fixture: Fixture) -> String {
        let mut grid = vec![vec![0_i64; 24]; 7];
        if matches!(fixture, Fixture::Workstation) {
            grid[0][9] = 900;
            grid[2][14] = 1_800;
            grid[4][18] = 3_600;
        }
        json!({ "grid": grid, "max_seconds": 3_600 }).to_string()
    }

    fn weekday_weekend_payload(fixture: Fixture) -> String {
        let active = matches!(fixture, Fixture::Workstation);
        json!({
            "weekday_total_seconds": if active { 7_200 } else { 0 },
            "weekday_times": if active { 6 } else { 0 },
            "weekday_mean_seconds": if active { 1_200 } else { 0 },
            "weekend_total_seconds": if active { 1_800 } else { 0 },
            "weekend_times": if active { 2 } else { 0 },
            "weekend_mean_seconds": if active { 900 } else { 0 },
            "weekday_total_hr": if active { "02 h 00 s" } else { "0 s" },
            "weekday_mean_hr": if active { "20 m 00 s" } else { "0 s" },
            "weekend_total_hr": if active { "30 m 00 s" } else { "0 s" },
            "weekend_mean_hr": if active { "15 m 00 s" } else { "0 s" }
        })
        .to_string()
    }

    fn options(server: &TestServer, client_id: &str) -> TuiOptions {
        TuiOptions {
            server_url: server.url.clone(),
            username: "admin".to_string(),
            password: "secret".to_string(),
            initial_client_id: client_id.to_string(),
            initial_alias: String::new(),
            timezone: "UTC".to_string(),
            page_size: 25,
            verbose_log: None,
        }
    }

    #[test]
    fn app_loads_stats_records_sessions_and_tokens() {
        let server = TestServer::new(Fixture::Workstation);

        let app = App::new(options(&server, "workstation")).unwrap();
        assert_eq!(app.current_client(), "workstation");
        assert_eq!(app.overview.as_ref().unwrap().stats.total.total_times, 1);
        assert_eq!(app.records.len(), 1);
        assert_eq!(app.records[0].status_label(), "completed");
        assert!(!app.legacy_records_api);
        assert_eq!(app.active.len(), 1);
        assert_eq!(app.tokens.len(), 1);
        assert!(app.client_ids.iter().any(|client| client == "other"));
        let analytics = app.analytics.as_ref().unwrap();
        assert_eq!(analytics.daily.len(), 4);
        assert_eq!(analytics.monthly.len(), 2);
        assert_eq!(analytics.hourly.grid.len(), 7);
        assert_eq!(analytics.weekday_weekend.weekday_times, 6);
    }

    #[test]
    fn alias_filter_cycles_and_filters_records_and_active_sessions() {
        let server = TestServer::new(Fixture::Workstation);
        let mut app = App::new(options(&server, "workstation")).unwrap();

        assert_eq!(app.current_alias(), "");
        assert_eq!(app.records.len(), 1);
        assert_eq!(app.active.len(), 1);

        app.cycle_alias(true);
        assert_eq!(app.current_alias(), "development");
        assert_eq!(app.records.len(), 1);
        assert_eq!(app.active.len(), 1);
        let requests = server.state.requests.lock().unwrap();
        for route in [
            "/api/stats",
            "/api/stats/streaks",
            "/api/stats/session-distribution",
            "/api/daily-data",
            "/api/stats/monthly-trend",
            "/api/stats/hourly-heatmap",
            "/api/stats/weekday-weekend",
            "/api/records",
        ] {
            assert!(requests.iter().any(|request| {
                request.split('?').next() == Some(route) && request.contains("alias=development")
            }));
        }
        drop(requests);

        app.cycle_alias(true);
        assert_eq!(app.current_alias(), "meetings");
        assert!(app.records.is_empty());
        assert!(app.active.is_empty());

        app.cycle_alias(true);
        assert_eq!(app.current_alias(), "");
    }

    #[test]
    fn initial_alias_is_preserved_even_when_not_returned_by_server() {
        let server = TestServer::new(Fixture::Empty);
        let mut options = options(&server, GLOBAL_CLIENT);
        options.initial_alias = "custom alias".to_string();

        let app = App::new(options).unwrap();
        assert_eq!(app.current_alias(), "custom alias");
        assert!(app.aliases.iter().any(|alias| alias == "custom alias"));
    }

    #[test]
    fn missing_status_from_legacy_server_is_inferred_with_warning() {
        let server = TestServer::new(Fixture::Legacy);
        let app = App::new(options(&server, "network")).unwrap();

        assert_eq!(app.records.len(), 1);
        assert_eq!(app.records[0].status, None);
        assert_eq!(app.records[0].status_label(), "completed");
        assert!(app.legacy_records_api);
        let notice = app.notice.as_ref().unwrap();
        assert!(notice.is_warning);
        assert!(notice.text.contains("omits record status"));
    }

    #[test]
    fn all_views_render_at_the_minimum_supported_size() {
        let server = TestServer::new(Fixture::Empty);
        let mut app = App::new(options(&server, GLOBAL_CLIENT)).unwrap();
        let backend = TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend).unwrap();

        for view in View::ALL {
            app.view = view;
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
        app.view = View::Analytics;
        for kind in AnalyticsKind::ALL {
            app.analytics_kind = kind;
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
        app.overlay = Some(Overlay::Help);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.overlay = Some(Overlay::Confirm(AdminAction::DeleteRecord(1)));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.overlay = Some(Overlay::TokenForm {
            field: TokenField::ClientId,
            client_id: "client".to_string(),
            description: String::new(),
        });
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.overlay = Some(Overlay::TokenCreated("tspan_test".to_string()));
        terminal.draw(|frame| app.draw(frame)).unwrap();
    }

    #[test]
    fn populated_analytics_graphs_render() {
        let server = TestServer::new(Fixture::Workstation);
        let mut app = App::new(options(&server, "workstation")).unwrap();
        app.view = View::Analytics;
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        app.analytics_kind = AnalyticsKind::Calendar;
        app.handle_view_key(KeyCode::Char('j'));
        assert_eq!(app.calendar_offset_weeks, 26);
        app.handle_view_key(KeyCode::Char('k'));
        assert_eq!(app.calendar_offset_weeks, 0);

        for kind in AnalyticsKind::ALL {
            app.analytics_kind = kind;
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            let expected = match kind {
                AnalyticsKind::Calendar => "Activity ·",
                AnalyticsKind::Monthly => "Duration",
                AnalyticsKind::Hourly => "Hourly heatmap",
                AnalyticsKind::Patterns => "Weekday vs weekend",
            };
            assert!(
                rendered.contains(expected),
                "missing {expected} chart title"
            );
        }
    }

    #[test]
    fn confirmed_admin_actions_refresh_the_loaded_data() {
        let server = TestServer::new(Fixture::Actions);
        let mut app = App::new(options(&server, GLOBAL_CLIENT)).unwrap();

        app.perform_action(AdminAction::DeleteRecord(1));
        assert!(app.records.iter().all(|record| record.id != 1));

        app.perform_action(AdminAction::EndSession(2));
        assert!(app.active.iter().all(|record| record.id != 2));
        assert!(app.records.iter().any(|record| record.id == 2));

        app.perform_action(AdminAction::RevokeToken("tspan_revoke_me".to_string()));
        assert!(app.tokens.is_empty());
    }

    #[test]
    fn authentication_failures_are_reported_clearly() {
        let server = TestServer::new(Fixture::Empty);
        let mut options = options(&server, GLOBAL_CLIENT);
        options.password = "wrong".to_string();

        let error = match App::new(options) {
            Ok(_) => panic!("invalid credentials unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("authentication failed"));
    }

    #[test]
    fn verbose_mode_writes_raw_responses_to_a_file() {
        let server = TestServer::new(Fixture::Empty);
        let path = std::env::temp_dir().join(format!(
            "tspan-tui-api-test-{}-{}.log",
            std::process::id(),
            server.address.port()
        ));
        let mut options = options(&server, GLOBAL_CLIENT);
        options.verbose_log = Some(path.clone());

        let app = App::new(options).unwrap();
        drop(app);
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains("[tspan-tui] --> GET"));
        assert!(log.contains("[tspan-tui] <-- HTTP 200"));
        assert!(log.contains("[tspan-tui] raw response body:\n[]"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn selection_wraps_and_handles_empty_lists() {
        let mut state = TableState::default();
        select_next(&mut state, 3);
        assert_eq!(state.selected(), Some(0));
        select_previous(&mut state, 3);
        assert_eq!(state.selected(), Some(2));
        select_next(&mut state, 0);
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn page_count_never_drops_below_one() {
        assert_eq!(total_pages(0, 25), 1);
        assert_eq!(total_pages(25, 25), 1);
        assert_eq!(total_pages(26, 25), 2);
    }

    #[test]
    fn token_redaction_keeps_context() {
        assert_eq!(redact_token("tspan_1234567890abcdef"), "tspan_1234…cdef");
        assert_eq!(redact_token("short-token"), "short-token");
    }

    #[test]
    fn verbose_url_includes_query_parameters() {
        assert_eq!(
            display_url_with_query(
                "https://example.test/api/stats",
                &[
                    ("client_id", "workstation".to_string()),
                    ("tz", "UTC".to_string())
                ]
            ),
            "https://example.test/api/stats?client_id=workstation&tz=UTC"
        );
    }
}
