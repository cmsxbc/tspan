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
    pub description: Option<String>,
    pub created_at: i64,
}

pub fn init_db(conn: &mut Connection) -> SqlResult<()> {
    // WAL mode must be set outside of a transaction
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
            alias           TEXT,
            process_id      INTEGER,
            status          TEXT DEFAULT 'active' CHECK (status IN ('active', 'completed', 'discarded')),
            created_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_records_client ON records(client_id);
        CREATE INDEX IF NOT EXISTS idx_records_start   ON records(start_time);
        CREATE INDEX IF NOT EXISTS idx_records_end     ON records(end_time);"
    )?;
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

pub fn start_session(
    conn: &mut Connection,
    client_id: &str,
    command: Option<&str>,
    alias: Option<&str>,
    process_id: Option<i64>,
) -> SqlResult<i64> {
    ensure_client(conn, client_id)?;
    let now = Utc::now().timestamp();
    conn.execute(
        "INSERT INTO records (client_id, start_time, command, alias, process_id, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?2)",
        params![client_id, now, command, alias, process_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn end_session(conn: &mut Connection, id: i64) -> SqlResult<Option<i64>> {
    let now = Utc::now().timestamp();
    let updated = conn.execute(
        "UPDATE records
         SET end_time = ?1,
             duration_seconds = ?1 - start_time,
             status = 'completed'
         WHERE id = ?2 AND status = 'active'",
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

pub fn discard_session(conn: &mut Connection, id: i64) -> SqlResult<bool> {
    let updated = conn.execute(
        "UPDATE records SET status = 'discarded' WHERE id = ?1 AND status = 'active'",
        params![id],
    )?;
    Ok(updated > 0)
}

pub fn get_orphaned_sessions(conn: &mut Connection) -> SqlResult<Vec<Record>> {
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

pub fn verify_api_token(conn: &mut Connection, token: &str) -> SqlResult<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM api_tokens WHERE token = ?1",
        params![token],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn list_api_tokens(conn: &mut Connection) -> SqlResult<Vec<ApiToken>> {
    let mut stmt = conn.prepare(
        "SELECT token, description, created_at FROM api_tokens ORDER BY created_at DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ApiToken {
            token: row.get(0)?,
            description: row.get(1)?,
            created_at: row.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn add_api_token(conn: &mut Connection, token: &str, description: Option<&str>) -> SqlResult<()> {
    let now = Utc::now().timestamp();
    conn.execute(
        "INSERT INTO api_tokens (token, description, created_at) VALUES (?1, ?2, ?3)",
        params![token, description, now],
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

pub fn import_record(
    conn: &mut Connection,
    client_id: &str,
    start_time: i64,
    end_time: i64,
    duration_seconds: i64,
) -> SqlResult<()> {
    ensure_client(conn, client_id)?;
    conn.execute(
        "INSERT INTO records (client_id, start_time, end_time, duration_seconds, status, created_at)
         VALUES (?1, ?2, ?3, ?4, 'completed', ?2)",
        params![client_id, start_time, end_time, duration_seconds],
    )?;
    Ok(())
}
