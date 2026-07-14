use crate::api_types::{
    human_readable_time, AliasStat, ClientStat, CommandStat, CreateTokenReq, CreateTokenResp,
    EndSessionResp, OrphanedSession, RecordPageItem, RecordsPageResp, SessionDistribution, Stats,
    StreakStats,
};
use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Utc};
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
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap},
    Frame, Terminal,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const GLOBAL_CLIENT: &str = "__global__";

pub struct TuiOptions {
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub initial_client_id: String,
    pub timezone: String,
    pub page_size: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Overview,
    Breakdown,
    Records,
    Active,
    Tokens,
}

impl View {
    const ALL: [Self; 5] = [
        Self::Overview,
        Self::Breakdown,
        Self::Records,
        Self::Active,
        Self::Tokens,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Breakdown => "Breakdown",
            Self::Records => "Records",
            Self::Active => "Active",
            Self::Tokens => "Tokens",
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
}

struct ApiClient {
    agent: ureq::Agent,
    base_url: String,
    authorization: String,
}

impl ApiClient {
    fn new(server_url: &str, username: &str, password: &str) -> Result<Self> {
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
                .build(),
        );
        Ok(Self {
            agent,
            base_url,
            authorization: format!("Basic {encoded}"),
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
        let mut request = self
            .agent
            .get(self.endpoint(path))
            .header("Authorization", &self.authorization);
        for (key, value) in query {
            request = request.query(key, value);
        }
        let mut response = request.call().map_err(|error| api_error(action, error))?;
        let body = response
            .body_mut()
            .read_to_string()
            .with_context(|| format!("{action}: could not read server response"))?;
        serde_json::from_str(&body)
            .with_context(|| format!("{action}: server returned invalid JSON"))
    }

    fn overview(&self, client_id: &str, timezone: Tz) -> Result<OverviewData> {
        let client = client_id.to_string();
        let tz = timezone.to_string();
        let filters = [("client_id", client.clone()), ("tz", tz)];
        let grouped_filter = [("client_id", client.clone())];
        let command_filter = [("client_id", client), ("depth", "1".to_string())];
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

    fn clients(&self) -> Result<Vec<String>> {
        self.get_json("clients", &[], "load clients")
    }

    fn records(&self, client_id: &str, page: i64, page_size: i64) -> Result<RecordsPageResp> {
        self.get_json(
            "records",
            &[
                ("client_id", client_id.to_string()),
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
        let request = self
            .agent
            .post(self.endpoint(&format!("sessions/{id}/end")))
            .header("Authorization", &self.authorization);
        match request.send_empty() {
            Ok(mut response) => {
                let body = response
                    .body_mut()
                    .read_to_string()
                    .context("end session: could not read server response")?;
                let result: EndSessionResp = serde_json::from_str(&body)
                    .context("end session: server returned invalid JSON")?;
                Ok(Some(result.duration_seconds))
            }
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(error) => Err(api_error("end session", error)),
        }
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
        let mut response = self
            .agent
            .post(self.endpoint("admin/tokens"))
            .header("Authorization", &self.authorization)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|error| api_error("create token", error))?;
        let body = response
            .body_mut()
            .read_to_string()
            .context("create token: could not read server response")?;
        let result: CreateTokenResp =
            serde_json::from_str(&body).context("create token: server returned invalid JSON")?;
        Ok(result.token)
    }

    fn revoke_token(&self, token: &str) -> Result<bool> {
        self.delete(&format!("admin/tokens/{token}"), "revoke token")
    }

    fn delete(&self, path: &str, action: &str) -> Result<bool> {
        match self
            .agent
            .delete(self.endpoint(path))
            .header("Authorization", &self.authorization)
            .call()
        {
            Ok(_) => Ok(true),
            Err(ureq::Error::StatusCode(404)) => Ok(false),
            Err(error) => Err(api_error(action, error)),
        }
    }

    fn post_empty(&self, path: &str, action: &str) -> Result<bool> {
        match self
            .agent
            .post(self.endpoint(path))
            .header("Authorization", &self.authorization)
            .send_empty()
        {
            Ok(_) => Ok(true),
            Err(ureq::Error::StatusCode(404)) => Ok(false),
            Err(error) => Err(api_error(action, error)),
        }
    }
}

fn api_error(action: &str, error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::StatusCode(401) => {
            anyhow!("{action}: authentication failed (check --username and --password)")
        }
        ureq::Error::StatusCode(403) => anyhow!("{action}: administrator access is required"),
        ureq::Error::StatusCode(status) => anyhow!("{action}: server returned HTTP {status}"),
        error => anyhow!("{action}: {error}"),
    }
}

struct App {
    api: ApiClient,
    timezone: Tz,
    view: View,
    breakdown_kind: BreakdownKind,
    breakdown_offset: usize,
    client_ids: Vec<String>,
    client_index: usize,
    overview: Option<OverviewData>,
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
    should_quit: bool,
    last_refresh: Instant,
}

impl App {
    fn new(options: TuiOptions) -> Result<Self> {
        let timezone = options
            .timezone
            .parse::<Tz>()
            .with_context(|| format!("invalid time zone '{}'", options.timezone))?;
        let api = ApiClient::new(&options.server_url, &options.username, &options.password)?;
        let initial_client_id = if options.initial_client_id.trim().is_empty() {
            GLOBAL_CLIENT.to_string()
        } else {
            options.initial_client_id
        };
        let mut app = Self {
            api,
            timezone,
            view: View::Overview,
            breakdown_kind: BreakdownKind::Clients,
            breakdown_offset: 0,
            client_ids: vec![GLOBAL_CLIENT.to_string()],
            client_index: 0,
            overview: None,
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
            should_quit: false,
            last_refresh: Instant::now(),
        };
        app.refresh_client_ids(Some(&initial_client_id))?;
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

    fn refresh_all(&mut self) -> Result<()> {
        self.refresh_client_ids(None)?;
        let client_id = self.current_client().to_string();
        let old_record_selection = self.records_state.selected().unwrap_or(0);
        let old_active_selection = self.active_state.selected().unwrap_or(0);
        let old_token_selection = self.tokens_state.selected().unwrap_or(0);

        let overview = self.api.overview(&client_id, self.timezone)?;
        let mut record_page = self
            .api
            .records(&client_id, self.records_page, self.page_size)?;
        let pages = total_pages(record_page.total, self.page_size);
        if self.records_page > pages {
            self.records_page = pages;
            record_page = self
                .api
                .records(&client_id, self.records_page, self.page_size)?;
        }
        let mut active = self.api.active_sessions()?;
        let mut tokens = self.api.tokens()?;
        if client_id != GLOBAL_CLIENT {
            active.retain(|record| record.client_id == client_id);
            tokens.retain(|token| token.client_id == client_id);
        }

        self.overview = Some(overview);
        self.records = record_page.records;
        self.records_total = record_page.total;
        self.active = active;
        self.tokens = tokens;
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
        if let Err(error) = self.refresh_all() {
            self.set_error(error);
        } else {
            self.set_notice(format!("Showing {}", self.current_client_label()));
        }
    }

    fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: false,
        });
    }

    fn set_error(&mut self, error: impl std::fmt::Display) {
        self.notice = Some(Notice {
            text: error.to_string(),
            is_error: true,
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
            KeyCode::Char('r') => match self.refresh_all() {
                Ok(()) => self.set_notice("Data refreshed"),
                Err(error) => self.set_error(error),
            },
            KeyCode::Char(']') => self.cycle_client(true),
            KeyCode::Char('[') => self.cycle_client(false),
            _ => self.handle_view_key(key.code),
        }
    }

    fn handle_view_key(&mut self, code: KeyCode) {
        match self.view {
            View::Overview => {}
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
        let title = format!(
            " TSPAN Admin · {} · {} · {} ",
            self.api.label(),
            self.current_client_label(),
            self.timezone
        );
        let tabs = Tabs::new(titles)
            .select(self.view.index())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(Color::DarkGray)),
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
            View::Overview => "[ ] client  r refresh  Tab view  ? help  q quit",
            View::Breakdown => {
                "←/→ category  ↑/↓ scroll  [ ] client  r refresh  Tab view  ? help  q quit"
            }
            View::Records => {
                "↑/↓ select  ←/→ page  d delete  [ ] client  r refresh  Tab view  ? help  q quit"
            }
            View::Active => {
                "↑/↓ select  e end  d discard  [ ] client  r refresh  Tab view  ? help  q quit"
            }
            View::Tokens => {
                "↑/↓ select  n new  d revoke  [ ] client  r refresh  Tab view  ? help  q quit"
            }
        };
        let notice = self.notice.as_ref();
        let notice_style = if notice.is_some_and(|notice| notice.is_error) {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
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
                Cell::from(record.status.clone()),
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
                    Line::from("  1–5 / Tab   switch views"),
                    Line::from("  ↑/↓ / j/k   select or scroll"),
                    Line::from("  [ / ]       change client filter"),
                    Line::from("  r           refresh (automatic every 10s)"),
                    Line::from("  q / Ctrl-C  quit"),
                    Line::from("View actions"),
                    Line::from("  Breakdown   ←/→ category"),
                    Line::from("  Records     ←/→ page · d delete"),
                    Line::from("  Active      e end · d discard"),
                    Line::from("  Tokens      n new · d revoke"),
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
        Actions,
    }

    struct MockState {
        fixture: Fixture,
        record_deleted: AtomicBool,
        session_ended: AtomicBool,
        token_revoked: AtomicBool,
    }

    struct TestServer {
        address: SocketAddr,
        stop: Arc<AtomicBool>,
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
                record_deleted: AtomicBool::new(false),
                session_ended: AtomicBool::new(false),
                token_revoked: AtomicBool::new(false),
            });
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
            ("GET", "/api/admin/tokens") => (200, tokens_payload(state)),
            ("GET", "/api/records") => (200, records_payload(state)),
            ("GET", "/api/sessions/orphaned") => (200, active_payload(state)),
            ("GET", "/api/stats") => (200, stats_payload(state.fixture)),
            ("GET", "/api/stats/streaks") => (200, streaks_payload()),
            ("GET", "/api/stats/session-distribution") => (200, distribution_payload()),
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
            Fixture::Actions => json!(["client"]),
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

    fn records_payload(state: &MockState) -> String {
        let mut records = Vec::new();
        match state.fixture {
            Fixture::Workstation => records.push(json!({
                "id": 1,
                "client_id": "workstation",
                "alias": "development",
                "command": "cargo test",
                "start_time": 1_700_000_000,
                "end_time": 1_700_000_120,
                "duration_seconds": 120,
                "status": "completed"
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
                    "alias": null,
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
            Fixture::Empty | Fixture::Actions => json!([]),
        };
        active.to_string()
    }

    fn stats_payload(fixture: Fixture) -> String {
        let total_times = i64::from(matches!(fixture, Fixture::Workstation));
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

    fn distribution_payload() -> String {
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

    fn options(server: &TestServer, client_id: &str) -> TuiOptions {
        TuiOptions {
            server_url: server.url.clone(),
            username: "admin".to_string(),
            password: "secret".to_string(),
            initial_client_id: client_id.to_string(),
            timezone: "UTC".to_string(),
            page_size: 25,
        }
    }

    #[test]
    fn app_loads_stats_records_sessions_and_tokens() {
        let server = TestServer::new(Fixture::Workstation);

        let app = App::new(options(&server, "workstation")).unwrap();
        assert_eq!(app.current_client(), "workstation");
        assert_eq!(app.overview.as_ref().unwrap().stats.total.total_times, 1);
        assert_eq!(app.records.len(), 1);
        assert_eq!(app.records[0].status, "completed");
        assert_eq!(app.active.len(), 1);
        assert_eq!(app.tokens.len(), 1);
        assert!(app.client_ids.iter().any(|client| client == "other"));
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
}
