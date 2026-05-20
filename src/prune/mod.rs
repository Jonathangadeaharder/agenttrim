pub mod backup;
pub mod mcp;
pub mod skills;
pub mod subprocess;

pub use mcp::PruneMcpReport;
pub use skills::PruneSkillsReport;

use anyhow::Result;
use std::path::PathBuf;

use agentalign_shared::models::{ReportKind, UnusedReport};

/// Combined report from a full prune operation.
#[derive(Debug, Clone)]
pub struct PruneReport {
    /// Items successfully removed.
    pub removed: Vec<String>,
    /// Items skipped because they are protected.
    pub skipped_protected: Vec<String>,
    /// Items that errored during removal.
    pub skipped_error: Vec<(String, String)>,
    /// Path to the combined backup directory.
    pub backup_path: Option<PathBuf>,
}

/// Prune skills that are safe to purge.
///
/// Delegates to `skills::prune_skills` and maps the result
/// to a unified `PruneReport`.
pub fn prune_skills_unified(reports: &[UnusedReport], skills_root: &Path, dry_run: bool) -> Result<PruneReport> {
    // Resolve skills_root: default to ~/.agents/skills
    let root = if skills_root.as_os_str().is_empty() || skills_root == Path::new("") {
        dirs::home_dir()
            .map(|h| h.join(".agents").join("skills"))
            .unwrap_or_else(|| PathBuf::from("~/.agents/skills"))
    } else {
        skills_root.to_path_buf()
    };

    let skill_report = crate::prune::skills::prune_skills(reports, &root, dry_run)?;

    Ok(PruneReport {
        removed: skill_report.removed,
        skipped_protected: skill_report.skipped_protected,
        skipped_error: skill_report.skipped_error,
        backup_path: skill_report.backup_path,
    })
}

use std::path::Path;

/// Prune MCP servers that are safe to purge.
///
/// Delegates to `mcp::prune_mcp_servers` and maps the result
/// to a unified `PruneReport`.
pub fn prune_mcp_unified(reports: &[UnusedReport], config_path: &Path, dry_run: bool) -> Result<PruneReport> {
    let mcp_report = crate::prune::mcp::prune_mcp_servers(reports, config_path, dry_run)?;

    Ok(PruneReport {
        removed: mcp_report.removed,
        skipped_protected: mcp_report.skipped_protected,
        skipped_error: mcp_report.skipped_error,
        backup_path: mcp_report.backup_path,
    })
}

/// Unified prune entry point: routes candidates to the correct
/// pruner (skills vs MCP servers) based on report kind.
///
/// `agents_root` — path to `~/.agents/` directory.
/// `mcp_config_path` — path to canonical MCP config.
/// `reports` — list of unused reports (pre-validated via validation hook).
/// `dry_run` — if true, shows what would be pruned without deleting.
pub fn prune_from_report(
    reports: &[UnusedReport],
    agents_root: &Path,
    mcp_config_path: &Path,
    dry_run: bool,
) -> Result<PruneReport> {
    let mut removed = Vec::new();
    let mut skipped_protected = Vec::new();
    let mut skipped_error = Vec::new();
    let mut backup_path: Option<PathBuf> = None;

    let skill_reports: Vec<&UnusedReport> = reports
        .iter()
        .filter(|r| r.kind == ReportKind::Skill)
        .collect();

    let mcp_reports: Vec<&UnusedReport> = reports
        .iter()
        .filter(|r| r.kind == ReportKind::McpServer)
        .collect();

    // Prune skills
    if !skill_reports.is_empty() {
        let skills_root = agents_root.join("skills");
        let sr = skill_reports.iter().map(|r| (*r).clone()).collect::<Vec<_>>();
        match prune_skills_unified(&sr, &skills_root, dry_run) {
            Ok(pr) => {
                removed.extend(pr.removed);
                skipped_protected.extend(pr.skipped_protected);
                skipped_error.extend(pr.skipped_error);
                if pr.backup_path.is_some() {
                    backup_path = pr.backup_path;
                }
            }
            Err(e) => {
                skipped_error.push(("skills-batch".to_string(), e.to_string()));
            }
        }
    }

    // Prune MCP servers
    if !mcp_reports.is_empty() {
        let mr = mcp_reports.iter().map(|r| (*r).clone()).collect::<Vec<_>>();
        match prune_mcp_unified(&mr, mcp_config_path, dry_run) {
            Ok(pr) => {
                removed.extend(pr.removed);
                skipped_protected.extend(pr.skipped_protected);
                skipped_error.extend(pr.skipped_error);
                if pr.backup_path.is_some() {
                    backup_path = pr.backup_path;
                }
            }
            Err(e) => {
                skipped_error.push(("mcp-batch".to_string(), e.to_string()));
            }
        }
    }

    Ok(PruneReport {
        removed,
        skipped_protected,
        skipped_error,
        backup_path,
    })
}
