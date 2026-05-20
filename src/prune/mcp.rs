use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::analyze::safety_matrix::SafetyMatrix;
use crate::prune::backup;
use agentalign_shared::models::{McpServerDefinition, ReportKind, UnusedReport};

/// Remove MCP server entries from the canonical config file.
///
/// For each report marked safe_to_purge:
/// 1. Run safety gate check
/// 2. Create a backup of the config file
/// 3. Remove the server entry from the JSON config
///
/// Returns a report of what was removed, skipped, or errored.
pub fn prune_mcp_servers(
    reports: &[UnusedReport],
    config_path: &Path,
    dry_run: bool,
) -> Result<PruneMcpReport> {
    let mut removed = Vec::new();
    let mut skipped_protected = Vec::new();
    let mut skipped_error = Vec::new();
    let mut backup_path: Option<PathBuf> = None;

    // Filter for MCP server reports that are safe to purge
    let candidates: Vec<&UnusedReport> = reports
        .iter()
        .filter(|r| r.kind == ReportKind::McpServer && r.safe_to_purge)
        .collect();

    if candidates.is_empty() {
        return Ok(PruneMcpReport {
            removed,
            skipped_protected,
            skipped_error,
            backup_path: None,
        });
    }

    // Load the existing config
    let mut config = load_mcp_config(config_path)?;

    // Create backup before pruning (only if not dry run)
    if !dry_run && config_path.exists() {
        let config_parent = config_path.parent().unwrap_or(Path::new("."));
        match backup::create_backup(
            "pre-prune-mcp",
            &[config_parent],
        ) {
            Ok(bp) => backup_path = Some(bp),
            Err(e) => {
                skipped_error.push((
                    "config-backup".to_string(),
                    format!("Failed to create backup: {e}"),
                ));
            }
        }
    }

    for report in candidates {
        let id = &report.key_identifier;

        // Check safety matrix
        if SafetyMatrix::is_protected(id) {
            skipped_protected.push(id.clone());
            continue;
        }

        if !config.contains_key(id) {
            // Already removed from config — consider it done
            removed.push(id.clone());
            continue;
        }

        if dry_run {
            removed.push(id.clone());
            continue;
        }

        // Remove the entry from the config
        config.remove(id);
        removed.push(id.clone());
    }

    // Write updated config back (only if we actually removed something and not dry run)
    if !dry_run && !removed.is_empty() {
        let content = serde_json::to_string_pretty(&config)
            .context("Failed to serialize updated MCP config")?;
        std::fs::write(config_path, &content)
            .with_context(|| format!("Failed to write updated config to {:?}", config_path))?;
    }

    Ok(PruneMcpReport {
        removed,
        skipped_protected,
        skipped_error,
        backup_path,
    })
}

/// Load MCP config from a JSON file.
fn load_mcp_config(
    path: &Path,
) -> Result<HashMap<String, McpServerDefinition>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read MCP config at {:?}", path))?;

    // Try flat map
    if let Ok(map) = serde_json::from_str::<HashMap<String, McpServerDefinition>>(&content) {
        return Ok(map);
    }

    // Try nested under "mcp" key
    #[derive(serde::Deserialize)]
    struct Wrapper {
        mcp: HashMap<String, McpServerDefinition>,
    }
    if let Ok(wrapper) = serde_json::from_str::<Wrapper>(&content) {
        return Ok(wrapper.mcp);
    }

    anyhow::bail!("Cannot parse MCP config: unknown JSON structure at {:?}", path);
}

/// Report from pruning MCP servers.
#[derive(Debug, Clone)]
pub struct PruneMcpReport {
    /// Servers successfully removed from config.
    pub removed: Vec<String>,
    /// Servers skipped because they are protected.
    pub skipped_protected: Vec<String>,
    /// Servers that errored during removal.
    pub skipped_error: Vec<(String, String)>,
    /// Path to the backup created before pruning.
    pub backup_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_mcp_report(id: &str, safe: bool) -> UnusedReport {
        UnusedReport {
            key_identifier: id.to_string(),
            kind: ReportKind::McpServer,
            path_context: None,
            last_active_timestamp: Some(100_000),
            safe_to_purge: safe,
            reason: "Test".to_string(),
        }
    }

    fn make_server_def() -> McpServerDefinition {
        McpServerDefinition {
            transport: agentalign_shared::models::TransportType::Local,
            command: Some(vec!["/bin/sh".to_string()]),
            url: None,
            headers: None,
            env: None,
            enabled: Some(true),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn test_prune_mcp_dry_run() {
        let tmp = std::env::temp_dir().join("agenttrim_test_mcp_dry");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let config_path = tmp.join("mcp_config.json");

        let mut config = HashMap::new();
        config.insert("test-server".to_string(), make_server_def());
        let content = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&config_path, &content).unwrap();

        let report = make_mcp_report("test-server", true);
        let result = prune_mcp_servers(&[report], &config_path, true).unwrap();

        assert_eq!(result.removed.len(), 1);
        assert!(result.removed.contains(&"test-server".to_string()));

        // Config should be unchanged (dry run)
        let loaded = load_mcp_config(&config_path).unwrap();
        assert!(loaded.contains_key("test-server"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prune_mcp_protected_skipped() {
        let tmp = std::env::temp_dir().join("agenttrim_test_mcp_protected");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let config_path = tmp.join("mcp_config.json");

        let report = make_mcp_report("supabase", true);
        let result = prune_mcp_servers(&[report], &config_path, false).unwrap();

        assert!(result.skipped_protected.contains(&"supabase".to_string()));
        assert!(result.removed.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prune_mcp_non_candidate_skipped() {
        let tmp = std::env::temp_dir().join("agenttrim_test_mcp_noncand");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let config_path = tmp.join("mcp_config.json");

        let report = make_mcp_report("test-server", false);
        let result = prune_mcp_servers(&[report], &config_path, false).unwrap();

        assert!(result.removed.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_load_mcp_config_not_found() {
        let result = load_mcp_config(Path::new("/nonexistent/config.json")).unwrap();
        assert!(result.is_empty());
    }
}
