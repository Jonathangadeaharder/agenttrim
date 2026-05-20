pub mod ledger_reader;
pub mod mcp;
pub mod process_scanner;
pub mod safety_matrix;
pub mod skills;
pub mod static_scanner;
pub mod validation_hook;

pub use agentalign_shared::models::{UnusedReport, UsageEntry};
#[allow(unused_imports)]
pub use agentalign_shared::traits::TrimAnalyzer;

use anyhow::Result;
use std::path::Path;

use crate::analyze::mcp::McpAnalyzer;
use crate::analyze::process_scanner::{find_mcp_processes, McpProcess};
use crate::analyze::safety_matrix::SafetyMatrix;
use crate::analyze::skills::{RealUsageStore, SkillAnalyzer};
use crate::time_provider::TimeProvider;

/// Comprehensive report from a full analysis run.
#[derive(Debug, Clone)]
pub struct FullAnalysisReport {
    /// Reports for skills found under ~/.agents/skills/.
    pub skills: Vec<UnusedReport>,
    /// Reports for MCP servers from the canonical config.
    pub mcp_servers: Vec<UnusedReport>,
    /// Currently running MCP processes detected by the process scanner.
    pub processes: Vec<McpProcess>,
    /// Total number of items marked as safe to purge.
    pub total_candidates: usize,
    /// Number of items excluded from reports due to safety matrix protection.
    pub protected_ignored: usize,
}

/// Run the full analyzer pipeline.
///
/// Scans skills, MCP servers, and running processes, applying the
/// safety matrix and usage ledger cross-references.
///
/// `agents_root` — path to `~/.agents/` directory.
/// `mcp_config_path` — path to the canonical MCP config JSON file.
/// `projects_root` — optional path for static text reference scanning.
/// `threshold_days` — inactivity threshold (default 90).
pub fn run_full_analysis(
    tp: &dyn TimeProvider,
    agents_root: &Path,
    mcp_config_path: &Path,
    projects_root: Option<&Path>,
    threshold_days: u64,
) -> Result<FullAnalysisReport> {
    // Run skill analysis
    let store = RealUsageStore;
    let analyzer = SkillAnalyzer::new(tp, &store);
    let skill_reports = analyzer.analyze(agents_root, threshold_days, projects_root)?;

    // Run MCP server analysis
    let mcp_reports = McpAnalyzer::analyze(mcp_config_path, threshold_days)?;

    // Run process scanner
    // We load the MCP config to pass defined servers to the scanner
    let mcp_servers = load_mcp_server_map(mcp_config_path).unwrap_or_default();
    let processes = find_mcp_processes(&mcp_servers).unwrap_or_default();

    // Count candidates and protected
    let total_candidates = skill_reports
        .iter()
        .chain(mcp_reports.iter())
        .filter(|r| r.safe_to_purge)
        .count();

    let protected_ignored = skill_reports
        .iter()
        .chain(mcp_reports.iter())
        .filter(|r| !r.safe_to_purge && SafetyMatrix::is_protected(&r.key_identifier))
        .count();

    Ok(FullAnalysisReport {
        skills: skill_reports,
        mcp_servers: mcp_reports,
        processes,
        total_candidates,
        protected_ignored,
    })
}

/// Load MCP server definitions from the canonical config.
fn load_mcp_server_map(
    path: &Path,
) -> Result<std::collections::HashMap<String, agentalign_shared::models::McpServerDefinition>> {
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }
    let content = std::fs::read_to_string(path)?;

    // Try flat map
    if let Ok(map) =
        serde_json::from_str::<std::collections::HashMap<String, agentalign_shared::models::McpServerDefinition>>(
            &content,
        )
    {
        return Ok(map);
    }

    // Try nested under "mcp" key
    #[derive(serde::Deserialize)]
    struct Wrapper {
        mcp: std::collections::HashMap<String, agentalign_shared::models::McpServerDefinition>,
    }
    if let Ok(wrapper) = serde_json::from_str::<Wrapper>(&content) {
        return Ok(wrapper.mcp);
    }

    Ok(std::collections::HashMap::new())
}
