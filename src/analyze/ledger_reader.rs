use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::analyze::UsageEntry;

/// Tool-level usage entry for per-tool monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUsageEntry {
    pub server_id: String,
    pub tool_name: Option<String>,
    pub total_calls: u64,
    pub last_used_timestamp: i64,
}

/// Path to the SQLite usage database.
pub fn db_path() -> Result<PathBuf> {
    let base = dirs::home_dir()
        .context("failed to resolve home directory")?
        .join(".agents");
    Ok(base)
}

/// Full path to the SQLite database file.
fn db_file_path() -> Result<PathBuf> {
    Ok(db_path()?.join("usage.db"))
}

/// Open (or create) the SQLite database and ensure the schema exists.
pub fn open_db() -> Result<Connection> {
    let base = db_path()?;
    std::fs::create_dir_all(&base).context("failed to create ~/.agents directory")?;

    let conn =
        Connection::open(db_file_path()?).context("failed to open SQLite database at ~/.agents/usage.db")?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS usage_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            server_id TEXT NOT NULL,
            tool_name TEXT,
            agent TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT 'call',
            timestamp INTEGER NOT NULL,
            byte_cost INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_usage_server ON usage_log(server_id);
        CREATE INDEX IF NOT EXISTS idx_usage_ts ON usage_log(timestamp);
        ",
    )
    .context("failed to create usage_log schema")?;

    // Migrate existing databases: add tool_name column if missing
    let column_exists = conn
        .prepare("SELECT 1 FROM pragma_table_info('usage_log') WHERE name = 'tool_name'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(true);
    if !column_exists {
        conn.execute("ALTER TABLE usage_log ADD COLUMN tool_name TEXT", [])
            .context("failed to add tool_name column to existing usage_log table")?;
    }

    Ok(conn)
}

/// Log a usage event for a given server+agent combination.
pub fn log_usage(server_id: &str, tool_name: Option<&str>, agent: &str, byte_cost: Option<u64>) -> Result<()> {
    let conn = open_db()?;
    let now = chrono_now();

    conn.execute(
        "INSERT INTO usage_log (server_id, tool_name, agent, action, timestamp, byte_cost) VALUES (?1, ?2, ?3, 'call', ?4, ?5)",
        rusqlite::params![server_id, tool_name, agent, now, byte_cost.map(|c| c as i64)],
    )
    .context("failed to insert usage log entry")?;

    Ok(())
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Aggregate usage stats grouped by server_id.
pub fn get_usage_stats() -> Result<Vec<UsageEntry>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT server_id, COUNT(*) as total_calls, MAX(timestamp) as last_used, AVG(byte_cost) as avg_cost
             FROM usage_log
             GROUP BY server_id
             ORDER BY last_used DESC",
        )
        .context("failed to prepare usage stats query")?;

    let entries = stmt
        .query_map([], |row| {
            let server_id: String = row.get(0)?;
            let total_calls: i64 = row.get(1)?;
            let last_used: i64 = row.get(2)?;
            let avg_cost: Option<f64> = row.get(3)?;

            Ok(UsageEntry {
                server_id,
                last_used_timestamp: last_used,
                total_call_count: total_calls as u64,
                context_window_byte_cost: avg_cost.map(|c| c as u64),
            })
        })
        .context("failed to query usage stats")?;

    let mut results = Vec::new();
    for entry in entries {
        results.push(entry.context("failed to read usage row")?);
    }
    Ok(results)
}

/// Return server_ids that have not been used since the given timestamp.
pub fn get_unused_since(timestamp: i64) -> Result<Vec<String>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT server_id FROM usage_log
             GROUP BY server_id
             HAVING MAX(timestamp) < ?1",
        )
        .context("failed to prepare unused-since query")?;

    let servers = stmt
        .query_map([timestamp], |row| row.get::<_, String>(0))
        .context("failed to query unused servers")?;

    let mut results = Vec::new();
    for s in servers {
        results.push(s.context("failed to read server_id")?);
    }
    Ok(results)
}

/// Aggregate usage stats grouped by server_id + tool_name.
pub fn get_tool_usage_stats() -> Result<Vec<ToolUsageEntry>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT server_id, tool_name, COUNT(*) as total_calls, MAX(timestamp) as last_used
             FROM usage_log
             GROUP BY server_id, tool_name
             ORDER BY last_used DESC",
        )
        .context("failed to prepare tool usage stats query")?;

    let entries = stmt
        .query_map([], |row| {
            let server_id: String = row.get(0)?;
            let tool_name: Option<String> = row.get(1)?;
            let total_calls: i64 = row.get(2)?;
            let last_used: i64 = row.get(3)?;

            Ok(ToolUsageEntry {
                server_id,
                tool_name,
                total_calls: total_calls as u64,
                last_used_timestamp: last_used,
            })
        })
        .context("failed to query tool usage stats")?;

    entries.map(|e| e.context("failed to read tool usage row")).collect()
}

// ---------------------------------------------------------------------------
// JSON ledger support (supplementary)
// ---------------------------------------------------------------------------

/// A single skill usage entry from the JSON skill ledger.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillUsageEntry {
    pub used: bool,
    pub last_used: Option<String>,
    pub used_by: Vec<String>,
    pub times_used: u64,
}

/// Top-level structure of ~/.agents/.skill-usage.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillUsageFile {
    last_updated: String,
    skills: HashMap<String, SkillUsageEntry>,
}

/// Path to the JSON skill-usage ledger.
fn skill_usage_path() -> Result<PathBuf> {
    Ok(db_path()?.join(".skill-usage.json"))
}

/// Read the JSON skill-usage ledger. Returns empty map if file does not exist.
pub fn read_skill_usage() -> Result<HashMap<String, SkillUsageEntry>> {
    let path = skill_usage_path()?;
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read skill usage ledger at {:?}", path))?;

    let file: SkillUsageFile =
        serde_json::from_str(&content).context("failed to parse skill usage JSON ledger")?;

    Ok(file.skills)
}
