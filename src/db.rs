use chrono::Utc;
use rusqlite::{Connection, Result as SqlResult, params};
use std::sync::{Arc, Mutex};

pub type DbPool = Arc<Mutex<Connection>>;

#[derive(Debug, Clone)]
pub struct Client {
    pub id: String,
    pub name: Option<String>,
    pub created_at: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone)]
pub struct Record {
    pub id: i64,
    pub client_id: String,
    pub start_time: i64,
    pub end_time: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub command: Option<String>,
    pub alias: Option<String>,
    pub process_id: Option<i64>,
    pub status: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ApiToken {
    pub token: String,
    pub client_id: String,
    pub description: Option<String>,
    pub created_at: i64,
}

pub fn init_db(conn: &mut Connection) -> SqlResult<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS clients (
            id          TEXT PRIMARY KEY,
            name        TEXT,
            created_at  INTEGER NOT NULL,
            last_seen   INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS api_tokens (
            token       TEXT PRIMARY KEY,
            client_id   TEXT NOT NULL DEFAULT 'default',
            description TEXT,
            created_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS records (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            client_id       TEXT NOT NULL REFERENCES clients(id),
            start_time      INTEGER NOT NULL,
            end_time        INTEGER,
            duration_seconds INTEGER,
            command         TEXT,
            command_tokens  TEXT,
            alias           TEXT,
            process_id      INTEGER,
            status          TEXT DEFAULT 'active' CHECK (status IN ('active', 'completed', 'discarded')),
            created_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_records_client ON records(client_id);
        CREATE INDEX IF NOT EXISTS idx_records_start   ON records(start_time);
        CREATE INDEX IF NOT EXISTS idx_records_end     ON records(end_time);

        -- Note: if migrating from old schema without client_id,
        -- manually run: ALTER TABLE api_tokens ADD COLUMN client_id TEXT NOT NULL DEFAULT 'default';
    ")?;
    // Attempt to add command_tokens column for existing databases; ignore if already exists
    let _ = conn.execute("ALTER TABLE records ADD COLUMN command_tokens TEXT", []);
    Ok(())
}

pub fn create_pool(db_path: &str) -> SqlResult<DbPool> {
    let mut conn = Connection::open(db_path)?;
    init_db(&mut conn)?;
    Ok(Arc::new(Mutex::new(conn)))
}

pub fn ensure_client(conn: &mut Connection, client_id: &str) -> SqlResult<()> {
    let now = Utc::now().timestamp();
    conn.execute(
        "INSERT INTO clients (id, name, created_at, last_seen)
         VALUES (?1, ?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET last_seen = ?2",
        params![client_id, now],
    )?;
    Ok(())
}

pub fn command_to_tokens(command: &str) -> Option<String> {
    let tokens: Vec<String> = shlex::split(command)
        .unwrap_or_else(|| command.split_whitespace().map(String::from).collect());
    serde_json::to_string(&tokens).ok()
}

pub fn start_session(
    conn: &mut Connection,
    client_id: &str,
    command: Option<&str>,
    alias: Option<&str>,
    process_id: Option<i64>,
) -> SqlResult<i64> {
    ensure_client(conn, client_id)?;
    let now = Utc::now().timestamp();
    let tokens_json = command.and_then(|cmd| command_to_tokens(cmd));
    conn.execute(
        "INSERT INTO records (client_id, start_time, command, command_tokens, alias, process_id, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?2)",
        params![client_id, now, command, tokens_json.as_deref(), alias, process_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn end_session(conn: &mut Connection, id: i64, client_id: &str) -> SqlResult<Option<i64>> {
    let now = Utc::now().timestamp();
    let updated = conn.execute(
        "UPDATE records
         SET end_time = ?1,
             duration_seconds = ?1 - start_time,
             status = 'completed'
         WHERE id = ?2 AND status = 'active' AND client_id = ?3",
        params![now, id, client_id],
    )?;
    if updated == 0 {
        return Ok(None);
    }
    let duration: i64 = conn.query_row(
        "SELECT duration_seconds FROM records WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    Ok(Some(duration))
}

pub fn discard_session(conn: &mut Connection, id: i64, client_id: &str) -> SqlResult<bool> {
    let updated = conn.execute(
        "UPDATE records SET status = 'discarded' WHERE id = ?1 AND status = 'active' AND client_id = ?2",
        params![id, client_id],
    )?;
    Ok(updated > 0)
}

pub fn end_session_admin(conn: &mut Connection, id: i64) -> SqlResult<Option<i64>> {
    let now = Utc::now().timestamp();
    let updated = conn.execute(
        "UPDATE records SET end_time = ?1, duration_seconds = ?1 - start_time, status = 'completed' WHERE id = ?2 AND status = 'active'",
        params![now, id],
    )?;
    if updated == 0 {
        return Ok(None);
    }
    let duration: i64 = conn.query_row(
        "SELECT duration_seconds FROM records WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    Ok(Some(duration))
}

pub fn discard_session_admin(conn: &mut Connection, id: i64) -> SqlResult<bool> {
    let updated = conn.execute(
        "UPDATE records SET status = 'discarded' WHERE id = ?1 AND status = 'active'",
        params![id],
    )?;
    Ok(updated > 0)
}

pub fn get_orphaned_sessions(conn: &mut Connection, client_id: &str) -> SqlResult<Vec<Record>> {
    let mut stmt = conn.prepare(
        "SELECT id, client_id, start_time, end_time, duration_seconds,
                command, alias, process_id, status, created_at
         FROM records
         WHERE status = 'active' AND client_id = ?1
         ORDER BY start_time DESC"
    )?;
    let rows = stmt.query_map([client_id], |row| {
        Ok(Record {
            id: row.get(0)?,
            client_id: row.get(1)?,
            start_time: row.get(2)?,
            end_time: row.get(3)?,
            duration_seconds: row.get(4)?,
            command: row.get(5)?,
            alias: row.get(6)?,
            process_id: row.get(7)?,
            status: row.get(8)?,
            created_at: row.get(9)?,
        })
    })?;
    rows.collect()
}

pub fn get_orphaned_sessions_admin(conn: &mut Connection) -> SqlResult<Vec<Record>> {
    let mut stmt = conn.prepare(
        "SELECT id, client_id, start_time, end_time, duration_seconds,
                command, alias, process_id, status, created_at
         FROM records
         WHERE status = 'active'
         ORDER BY start_time DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Record {
            id: row.get(0)?,
            client_id: row.get(1)?,
            start_time: row.get(2)?,
            end_time: row.get(3)?,
            duration_seconds: row.get(4)?,
            command: row.get(5)?,
            alias: row.get(6)?,
            process_id: row.get(7)?,
            status: row.get(8)?,
            created_at: row.get(9)?,
        })
    })?;
    rows.collect()
}

fn record_from_row(row: &rusqlite::Row) -> SqlResult<Record> {
    Ok(Record {
        id: row.get(0)?,
        client_id: row.get(1)?,
        start_time: row.get(2)?,
        end_time: row.get(3)?,
        duration_seconds: row.get(4)?,
        command: row.get(5)?,
        alias: row.get(6)?,
        process_id: row.get(7)?,
        status: row.get(8)?,
        created_at: row.get(9)?,
    })
}

pub fn distinct_client_ids(conn: &mut Connection) -> SqlResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT client_id FROM records ORDER BY client_id")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

#[derive(serde::Serialize)]
pub struct ClientInfo {
    pub id: String,
    pub name: Option<String>,
    pub created_at: i64,
    pub last_seen: i64,
}

pub fn list_clients(conn: &mut Connection) -> SqlResult<Vec<ClientInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at, last_seen FROM clients ORDER BY last_seen DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ClientInfo {
            id: row.get(0)?,
            name: row.get(1)?,
            created_at: row.get(2)?,
            last_seen: row.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn list_records_page(
    conn: &mut Connection,
    client_id: &str,
    alias_filter: &str,
    command_filter: &str,
    page: i64,
    per_page: i64,
) -> SqlResult<(Vec<Record>, i64)> {
    let offset = (page - 1) * per_page;
    let mut conditions = vec!["status != 'active'"];
    let mut param_refs: Vec<&dyn rusqlite::ToSql> = Vec::new();
    let alias_like;
    let command_like;

    if !client_id.is_empty() && client_id != "__global__" {
        conditions.push("client_id = ?");
        param_refs.push(&client_id);
    }
    if !alias_filter.is_empty() {
        conditions.push("alias LIKE ?");
        alias_like = format!("%{}%", alias_filter);
        param_refs.push(&alias_like);
    }
    if !command_filter.is_empty() {
        conditions.push("command LIKE ?");
        command_like = format!("%{}%", command_filter);
        param_refs.push(&command_like);
    }

    let where_clause = conditions.join(" AND ");

    let count_sql = format!("SELECT COUNT(*) FROM records WHERE {}", where_clause);
    let total: i64 = conn.query_row(
        &count_sql,
        rusqlite::params_from_iter(&param_refs),
        |row| row.get(0),
    )?;

    let mut select_params = param_refs.clone();
    select_params.push(&per_page);
    select_params.push(&offset);

    let select_sql = format!(
        "SELECT id, client_id, start_time, end_time, duration_seconds,
                command, alias, process_id, status, created_at
         FROM records
         WHERE {}
         ORDER BY start_time DESC
         LIMIT ? OFFSET ?",
        where_clause
    );
    let mut stmt = conn.prepare(&select_sql)?;
    let records: Vec<Record> = stmt.query_map(
        rusqlite::params_from_iter(&select_params),
        record_from_row,
    )?.collect::<SqlResult<Vec<_>>>()?;

    Ok((records, total))
}

pub fn verify_api_token(conn: &mut Connection, token: &str) -> SqlResult<(bool, String)> {
    let result: Option<String> = conn.query_row(
        "SELECT client_id FROM api_tokens WHERE token = ?1",
        params![token],
        |row| row.get(0),
    ).ok();
    match result {
        Some(client_id) => Ok((true, client_id)),
        None => Ok((false, String::new())),
    }
}

pub fn list_api_tokens(conn: &mut Connection) -> SqlResult<Vec<ApiToken>> {
    let mut stmt = conn.prepare(
        "SELECT token, client_id, description, created_at FROM api_tokens ORDER BY created_at DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ApiToken {
            token: row.get(0)?,
            client_id: row.get(1)?,
            description: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn add_api_token(conn: &mut Connection, token: &str, client_id: &str, description: Option<&str>) -> SqlResult<()> {
    let now = Utc::now().timestamp();
    conn.execute(
        "INSERT INTO api_tokens (token, client_id, description, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![token, client_id, description, now],
    )?;
    Ok(())
}

pub fn delete_api_token(conn: &mut Connection, token: &str) -> SqlResult<bool> {
    let deleted = conn.execute(
        "DELETE FROM api_tokens WHERE token = ?1",
        params![token],
    )?;
    Ok(deleted > 0)
}

pub fn delete_record(conn: &mut Connection, id: i64) -> SqlResult<bool> {
    let deleted = conn.execute(
        "DELETE FROM records WHERE id = ?1",
        params![id],
    )?;
    Ok(deleted > 0)
}

pub fn import_record(
    conn: &mut Connection,
    client_id: &str,
    start_time: i64,
    end_time: i64,
    duration_seconds: i64,
    command: Option<&str>,
    alias: Option<&str>,
) -> SqlResult<()> {
    ensure_client(conn, client_id)?;
    let tokens_json = command.and_then(|cmd| command_to_tokens(cmd));
    conn.execute(
        "INSERT INTO records (client_id, start_time, end_time, duration_seconds, command, command_tokens, alias, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'completed', ?2)",
        params![client_id, start_time, end_time, duration_seconds, command, tokens_json.as_deref(), alias],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&mut conn).unwrap();
        conn
    }

    #[test]
    fn test_command_to_tokens_simple() {
        assert_eq!(
            command_to_tokens("perf stats record"),
            Some(r#"["perf","stats","record"]"#.to_string())
        );
    }

    #[test]
    fn test_command_to_tokens_quoted() {
        assert_eq!(
            command_to_tokens("ls \"xxx yyy\""),
            Some(r#"["ls","xxx yyy"]"#.to_string())
        );
    }

    #[test]
    fn test_command_to_tokens_single_quoted() {
        assert_eq!(
            command_to_tokens("echo 'hello world'"),
            Some(r#"["echo","hello world"]"#.to_string())
        );
    }

    #[test]
    fn test_command_to_tokens_unclosed_quote_fallback() {
        // shlex returns None for unclosed quote, falls back to split_whitespace
        let result = command_to_tokens("echo \"hello").unwrap();
        assert!(result.contains("echo"));
        assert!(result.contains("\"hello"));
    }

    #[test]
    fn test_command_to_tokens_empty() {
        assert_eq!(command_to_tokens(""), Some(r#"[]"#.to_string()));
    }

    #[test]
    fn test_start_session_stores_tokens() {
        let mut conn = setup();
        let id = start_session(&mut conn, "test-client", Some("python train.py --epochs 10"), None, None).unwrap();

        let tokens: String = conn.query_row(
            "SELECT command_tokens FROM records WHERE id = ?1",
            params![id],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(tokens, r#"["python","train.py","--epochs","10"]"#);
    }

    #[test]
    fn test_start_session_null_command_no_tokens() {
        let mut conn = setup();
        let id = start_session(&mut conn, "test-client", None, Some("alias-only"), None).unwrap();

        let tokens: Option<String> = conn.query_row(
            "SELECT command_tokens FROM records WHERE id = ?1",
            params![id],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(tokens, None);
    }

    #[test]
    fn test_import_record_stores_command_and_tokens() {
        let mut conn = setup();
        import_record(&mut conn, "imported", 1609459200, 1609459260, 60, Some("vim file.txt"), Some("editing")).unwrap();

        let (cmd, tokens, alias): (Option<String>, Option<String>, Option<String>) = conn.query_row(
            "SELECT command, command_tokens, alias FROM records WHERE client_id = ?1",
            params!["imported"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();

        assert_eq!(cmd, Some("vim file.txt".to_string()));
        assert_eq!(tokens, Some(r#"["vim","file.txt"]"#.to_string()));
        assert_eq!(alias, Some("editing".to_string()));
    }

    #[test]
    fn test_import_record_null_command() {
        let mut conn = setup();
        import_record(&mut conn, "imported", 1609459200, 1609459260, 60, None, None).unwrap();

        let (cmd, tokens, alias): (Option<String>, Option<String>, Option<String>) = conn.query_row(
            "SELECT command, command_tokens, alias FROM records WHERE client_id = ?1",
            params!["imported"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();

        assert_eq!(cmd, None);
        assert_eq!(tokens, None);
        assert_eq!(alias, None);
    }
}
