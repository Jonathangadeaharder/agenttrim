use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use crate::analyze::ledger_reader;
use crate::analyze::safety_matrix::SafetyMatrix;
use agentalign_shared::models::{McpServerDefinition, ReportKind, UnusedReport, UsageEntry};

/// Health status of an MCP server binary.
#[derive(Debug, Clone, PartialEq)]
pub enum HealthStatus {
    /// Binary exists and is executable.
    Healthy,
    /// Binary path no longer exists on filesystem.
    BinaryMissing(String),
    /// Server has zero recorded calls in telemetry.
    #[allow(dead_code)]
    ZeroCalls,
    /// Server's root filesystem path is overly permissive.
    DeadPath(String),
}

/// Analyzes configured MCP servers against usage telemetry,
/// file system health, and structural issues.
pub struct McpAnalyzer;

impl McpAnalyzer {
    /// Analyze configured MCP servers against usage and health.
    ///
    /// `canonical_path` — path to the canonical MCP config file
    /// (typically `~/.agents/mcp_config.json`).
    pub fn analyze(
        canonical_path: &Path,
        threshold_days: u64,
    ) -> Result<Vec<UnusedReport>> {
        let servers = Self::load_mcp_config(canonical_path)?;

        if servers.is_empty() {
            return Ok(Vec::new());
        }

        // Load usage stats from SQLite
        let usage_stats = ledger_reader::get_usage_stats().unwrap_or_default();

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let threshold_secs = (threshold_days as i64) * 86_400;
        let cutoff = now_secs - threshold_secs;

        // Check for duplicates
        let duplicates = Self::find_duplicates(&servers);

        let mut reports = Vec::new();

        for (server_id, definition) in &servers {
            // Check safety matrix first
            if SafetyMatrix::is_protected(server_id) {
                let reason = SafetyMatrix::protection_reason(server_id)
                    .unwrap_or("Protected by safety matrix");
                reports.push(UnusedReport {
                    key_identifier: server_id.clone(),
                    kind: ReportKind::McpServer,
                    path_context: None,
                    last_active_timestamp: None,
                    safe_to_purge: false,
                    reason: format!("Protected: {reason}"),
                });
                continue;
            }

            // Check usage stats
            let usage_info = Self::get_server_usage(server_id, &usage_stats);

            // Check binary health
            let health = Self::check_server_health(definition);

            // Check for duplicate flags
            let is_duplicate = duplicates.contains(server_id);

            // Determine safe_to_purge
            let is_unused = usage_info
                .last_used
                .map(|t| t < cutoff)
                .unwrap_or(true);

            let flag_reasons = Self::collect_health_reasons(&health, is_unused, is_duplicate);

            let safe_to_purge = is_unused
                && health != HealthStatus::Healthy
                && !is_duplicate;

            let reason = if flag_reasons.is_empty() {
                if is_unused {
                    "No usage in telemetry within threshold period but binary healthy"
                } else {
                    "Still actively used"
                }
            } else {
                &flag_reasons.join("; ")
            };

            reports.push(UnusedReport {
                key_identifier: server_id.clone(),
                kind: ReportKind::McpServer,
                path_context: Some(Self::server_path_context(definition)),
                last_active_timestamp: usage_info.last_used,
                safe_to_purge,
                reason: reason.to_string(),
            });
        }

        Ok(reports)
    }

    /// Check if server binary still exists at configured path.
    pub fn check_server_health(server: &McpServerDefinition) -> HealthStatus {
        // Check command-based servers
        if let Some(ref cmd) = server.command {
            if let Some(binary_path) = cmd.first() {
                let path = Path::new(binary_path);
                if !path.exists() {
                    // Maybe it's a name in PATH, check `which`-style
                    if !Self::is_in_path(binary_path) {
                        return HealthStatus::BinaryMissing(binary_path.clone());
                    }
                }
            }
        }

        // Check for overly permissive filesystem paths in env
        if let Some(ref env) = server.env {
            for (_key, value) in env {
                let p = Path::new(value);
                if p == Path::new("/") || p == Path::new("~") || value == "/" || value == "~" {
                    return HealthStatus::DeadPath(value.clone());
                }
            }
        }

        // Check URL-based servers
        if let Some(ref url) = server.url {
            if url.is_empty() || url == "http://localhost:0" || url == "tcp://0.0.0.0:0" {
                return HealthStatus::DeadPath(url.clone());
            }
        }

        // Check if zero calls (usage stats are checked separately)
        HealthStatus::Healthy
    }

    /// Check for duplicate/conflicting server definitions.
    ///
    /// Duplicate detection: same command binary, same URL endpoint,
    /// or same server ID across definitions.
    pub fn find_duplicates(servers: &HashMap<String, McpServerDefinition>) -> Vec<String> {
        let mut duplicate_ids = Vec::new();
        let mut seen_binaries: HashMap<&str, &str> = HashMap::new(); // binary -> first server_id
        let mut seen_urls: HashMap<&str, &str> = HashMap::new(); // url -> first server_id

        for (id, def) in servers {
            // Check command duplicates
            if let Some(ref cmd) = def.command {
                if let Some(bin) = cmd.first() {
                    if let Some(&first_id) = seen_binaries.get(bin.as_str()) {
                        if !duplicate_ids.iter().any(|x| x == id) {
                            duplicate_ids.push(id.clone());
                        }
                        if !duplicate_ids.iter().any(|x| x == first_id) {
                            duplicate_ids.push(first_id.to_string());
                        }
                    } else {
                        seen_binaries.insert(bin.as_str(), id);
                    }
                }
            }

            // Check URL duplicates
            if let Some(ref url) = def.url {
                if !url.is_empty() {
                    if let Some(&first_id) = seen_urls.get(url.as_str()) {
                        if !duplicate_ids.iter().any(|x| x == id) {
                            duplicate_ids.push(id.clone());
                        }
                        if !duplicate_ids.iter().any(|x| x == first_id) {
                            duplicate_ids.push(first_id.to_string());
                        }
                    } else {
                        seen_urls.insert(url.as_str(), id);
                    }
                }
            }
        }

        duplicate_ids
    }

    /// Parse the canonical MCP config JSON file.
    fn load_mcp_config(
        path: &Path,
    ) -> Result<HashMap<String, McpServerDefinition>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }

        let content =
            std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read MCP config at {:?}", path))?;

        // Try parsing as a flat map first
        if let Ok(map) = serde_json::from_str::<HashMap<String, McpServerDefinition>>(&content) {
            return Ok(map);
        }

        // Try parsing as CanonicalWorkspaceState (nested under "mcp")
        #[derive(serde::Deserialize)]
        struct Wrapper {
            mcp: HashMap<String, McpServerDefinition>,
        }
        if let Ok(wrapper) = serde_json::from_str::<Wrapper>(&content) {
            return Ok(wrapper.mcp);
        }

        anyhow::bail!("Cannot parse MCP config: unknown JSON structure at {:?}", path);
    }

    /// Get usage information for a specific server from the usage ledger.
    fn get_server_usage(
        server_id: &str,
        usage_stats: &[UsageEntry],
    ) -> UsageInfo {
        let mut last_used: Option<i64> = None;
        let mut total_calls: u64 = 0;

        for entry in usage_stats {
            if entry.server_id == server_id {
                if last_used
                    .map(|lu| entry.last_used_timestamp > lu)
                    .unwrap_or(true)
                {
                    last_used = Some(entry.last_used_timestamp);
                }
                total_calls = total_calls.saturating_add(entry.total_call_count);
            }
        }

        UsageInfo {
            last_used,
            total_calls,
        }
    }

    /// Build a path context string for display.
    fn server_path_context(def: &McpServerDefinition) -> String {
        if let Some(ref cmd) = def.command {
            cmd.join(" ")
        } else if let Some(ref url) = def.url {
            url.clone()
        } else {
            String::from("unknown transport")
        }
    }

    /// Check if a binary name is available in PATH.
    fn is_in_path(binary: &str) -> bool {
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths).any(|dir| dir.join(binary).exists()).then_some(())
            })
            .is_some()
    }

    /// Build human-readable reasons from health status and flags.
    fn collect_health_reasons(
        health: &HealthStatus,
        is_unused: bool,
        is_duplicate: bool,
    ) -> Vec<String> {
        let mut reasons = Vec::new();

        match health {
            HealthStatus::Healthy => {}
            HealthStatus::BinaryMissing(path) => {
                reasons.push(format!("Binary not found: {path}"));
            }
            HealthStatus::ZeroCalls => {
                reasons.push("Zero recorded calls in telemetry".to_string());
            }
            HealthStatus::DeadPath(path) => {
                reasons.push(format!("Overly permissive path: {path}"));
            }
        }

        if is_unused {
            reasons.push("No usage within threshold period".to_string());
        }

        if is_duplicate {
            reasons.push("Duplicate server definition detected".to_string());
        }

        reasons
    }
}

/// Internal usage summary for a single MCP server.
#[allow(dead_code)]
struct UsageInfo {
    last_used: Option<i64>,
    total_calls: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stdio_server(command: &[&str]) -> McpServerDefinition {
        McpServerDefinition {
            transport: agentalign_shared::models::TransportType::Local,
            command: Some(command.iter().map(|s| s.to_string()).collect()),
            url: None,
            headers: None,
            env: None,
            enabled: Some(true),
            extra: HashMap::new(),
        }
    }

    fn make_remote_server(url: &str) -> McpServerDefinition {
        McpServerDefinition {
            transport: agentalign_shared::models::TransportType::Remote,
            command: None,
            url: Some(url.to_string()),
            headers: None,
            env: None,
            enabled: Some(true),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn test_check_health_healthy() {
        // Use an actual binary that exists (like /bin/sh)
        let server = make_stdio_server(&["/bin/sh"]);
        assert_eq!(McpAnalyzer::check_server_health(&server), HealthStatus::Healthy);
    }

    #[test]
    fn test_check_health_binary_missing() {
        let server = make_stdio_server(&["/usr/bin/nonexistent-binary-12345"]);
        let health = McpAnalyzer::check_server_health(&server);
        assert!(matches!(health, HealthStatus::BinaryMissing(_)));
    }

    #[test]
    fn test_check_health_dead_path_root() {
        let mut server = make_stdio_server(&["/bin/sh"]);
        let mut env = HashMap::new();
        env.insert("ROOT".to_string(), "/".to_string());
        server.env = Some(env);
        let health = McpAnalyzer::check_server_health(&server);
        assert!(matches!(health, HealthStatus::DeadPath(_)));
    }

    #[test]
    fn test_find_duplicates_by_binary() {
        let mut servers = HashMap::new();
        servers.insert("server-a".to_string(), make_stdio_server(&["/bin/node"]));
        servers.insert("server-b".to_string(), make_stdio_server(&["/bin/node"]));

        let dups = McpAnalyzer::find_duplicates(&servers);
        assert!(dups.contains(&"server-a".to_string()));
        assert!(dups.contains(&"server-b".to_string()));
    }

    #[test]
    fn test_find_duplicates_by_url() {
        let mut servers = HashMap::new();
        servers.insert(
            "remote-a".to_string(),
            make_remote_server("https://api.example.com/mcp"),
        );
        servers.insert(
            "remote-b".to_string(),
            make_remote_server("https://api.example.com/mcp"),
        );

        let dups = McpAnalyzer::find_duplicates(&servers);
        assert!(dups.contains(&"remote-a".to_string()));
        assert!(dups.contains(&"remote-b".to_string()));
    }

    #[test]
    fn test_no_duplicates_unique() {
        let mut servers = HashMap::new();
        servers.insert("server-a".to_string(), make_stdio_server(&["/bin/node"]));
        servers.insert("server-b".to_string(), make_stdio_server(&["/bin/python"]));

        let dups = McpAnalyzer::find_duplicates(&servers);
        assert!(dups.is_empty());
    }

    #[test]
    fn test_load_mcp_config_not_found() {
        let path = Path::new("/nonexistent/config.json");
        let servers = McpAnalyzer::load_mcp_config(path).unwrap();
        assert!(servers.is_empty());
    }
}
