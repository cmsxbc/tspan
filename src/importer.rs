use chrono::NaiveDateTime;
use crate::db::{DbPool, import_record};

pub struct ImportResult {
    pub imported: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

pub async fn import_from_directory(pool: &DbPool, client_id: &str, dir: &str) -> anyhow::Result<ImportResult> {
    let mut imported = 0;
    let mut failed = 0;
    let mut errors = vec![];

    let entries = std::fs::read_dir(dir)?;
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let p = e.path();
            p.extension().map(|ext| ext == "txt").unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();

    files.sort();

    for path in files {
        let filename = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                failed += 1;
                errors.push(format!("{}: read error {}", filename, e));
                continue;
            }
        };

        let lines: Vec<&str> = content.trim().split('\n').collect();
        if lines.len() < 2 {
            failed += 1;
            errors.push(format!("{}: insufficient lines", filename));
            continue;
        }

        let start_time = match NaiveDateTime::parse_from_str(filename, "%Y%m%d%H%M%S") {
            Ok(dt) => dt.and_utc().timestamp(),
            Err(e) => {
                failed += 1;
                errors.push(format!("{}: parse filename datetime error {}", filename, e));
                continue;
            }
        };

        let duration_seconds: i64 = match lines[1].trim().parse() {
            Ok(v) => v,
            Err(e) => {
                failed += 1;
                errors.push(format!("{}: parse duration error {}", filename, e));
                continue;
            }
        };

        let end_time = start_time + duration_seconds;
        let command = lines[0].trim();
        let command_opt = if command.is_empty() { None } else { Some(command) };

        let mut conn = pool.lock().unwrap();
        if let Err(e) = import_record(&mut conn, client_id, start_time, end_time, duration_seconds, command_opt) {
            failed += 1;
            errors.push(format!("{}: db error {}", filename, e));
        } else {
            imported += 1;
        }
    }

    Ok(ImportResult { imported, failed, errors })
}
