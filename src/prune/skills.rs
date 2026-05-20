use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::analyze::safety_matrix::SafetyMatrix;
use crate::prune::backup;
use agentalign_shared::models::{ReportKind, UnusedReport};

/// Remove skill directories and symlinks from `~/.agents/skills/`.
///
/// For each report marked safe_to_purge:
/// 1. Run safety gate check (protected items are skipped)
/// 2. Create a timestamped backup
/// 3. Remove the symlink or directory
///
/// Returns a report of what was removed, skipped, or errored.
pub fn prune_skills(
    reports: &[UnusedReport],
    _skills_root: &Path,
    dry_run: bool,
) -> Result<PruneSkillsReport> {
    let mut removed = Vec::new();
    let mut skipped_protected = Vec::new();
    let mut skipped_error = Vec::new();
    let mut backup_path: Option<PathBuf> = None;

    // Filter for skill reports that are safe to purge
    let candidates: Vec<&UnusedReport> = reports
        .iter()
        .filter(|r| r.kind == ReportKind::Skill && r.safe_to_purge)
        .collect();

    if candidates.is_empty() {
        return Ok(PruneSkillsReport {
            removed,
            skipped_protected,
            skipped_error,
            backup_path: None,
        });
    }

    // Collect paths for backup
    let candidate_paths: Vec<PathBuf> = candidates
        .iter()
        .filter_map(|r| {
            r.path_context
                .as_ref()
                .map(PathBuf::from)
                .filter(|p| p.exists())
        })
        .collect();

    // Create backup before pruning (only if not dry run)
    if !dry_run && !candidate_paths.is_empty() {
        let paths_refs: Vec<&Path> = candidate_paths.iter().map(|p| p.as_path()).collect();
        match backup::create_backup("pre-prune-skills", &paths_refs) {
            Ok(bp) => backup_path = Some(bp),
            Err(e) => {
                skipped_error.push((
                    "backup".to_string(),
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

        // Get the skill path from the report's path_context
        let skill_path = match &report.path_context {
            Some(p) => PathBuf::from(p),
            None => {
                skipped_error.push((id.clone(), "No path context in report".to_string()));
                continue;
            }
        };

        if !skill_path.exists() {
            // Already gone — consider it done
            removed.push(id.clone());
            continue;
        }

        if dry_run {
            removed.push(id.clone());
            continue;
        }

        // Remove the skill
        match remove_skill_path(&skill_path) {
            Ok(()) => removed.push(id.clone()),
            Err(e) => {
                skipped_error.push((id.clone(), format!("Removal failed: {e}")));
            }
        }
    }

    Ok(PruneSkillsReport {
        removed,
        skipped_protected,
        skipped_error,
        backup_path,
    })
}

/// Remove a skill directory or symlink.
fn remove_skill_path(path: &Path) -> Result<()> {
    if path.is_symlink() {
        // Remove symlink only (don't follow it)
        std::fs::remove_file(path)
            .with_context(|| format!("Failed to remove symlink: {}", path.display()))?;
    } else if path.is_dir() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove directory: {}", path.display()))?;
    } else {
        std::fs::remove_file(path)
            .with_context(|| format!("Failed to remove file: {}", path.display()))?;
    }
    Ok(())
}

/// Report from pruning skills.
#[derive(Debug, Clone)]
pub struct PruneSkillsReport {
    /// Skills successfully removed.
    pub removed: Vec<String>,
    /// Skills skipped because they are protected.
    pub skipped_protected: Vec<String>,
    /// Skills that errored during removal.
    pub skipped_error: Vec<(String, String)>,
    /// Path to the backup created before pruning.
    pub backup_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_skill_report(id: &str, path: &str, safe: bool) -> UnusedReport {
        UnusedReport {
            key_identifier: id.to_string(),
            kind: ReportKind::Skill,
            path_context: Some(path.to_string()),
            last_active_timestamp: Some(100_000),
            safe_to_purge: safe,
            reason: "Test".to_string(),
        }
    }

    #[test]
    fn test_prune_skills_dry_run() {
        let tmp = std::env::temp_dir().join("agenttrim_test_dry_run");
        let _ = fs::remove_dir_all(&tmp);
        let skills_dir = tmp.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        let skill_path = skills_dir.join("test-skill");
        fs::create_dir_all(&skill_path).unwrap();
        fs::write(skill_path.join("SKILL.md"), "test").unwrap();

        let report = make_skill_report("test-skill", skill_path.to_str().unwrap(), true);
        let result = prune_skills(&[report], &skills_dir, true).unwrap();

        assert_eq!(result.removed.len(), 1);
        assert!(result.removed.contains(&"test-skill".to_string()));
        assert!(skill_path.exists(), "Dry run should not remove files");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prune_skills_protected_skipped() {
        let report = make_skill_report("supabase", "/tmp/nonexistent", true);
        let result = prune_skills(&[report], Path::new("/tmp"), false).unwrap();
        assert!(result.skipped_protected.contains(&"supabase".to_string()));
        assert!(result.removed.is_empty());
    }

    #[test]
    fn test_prune_skills_non_candidate_skipped() {
        let report = make_skill_report("test-skill", "/tmp/nonexistent", false);
        let result = prune_skills(&[report], Path::new("/tmp"), false).unwrap();
        assert!(result.removed.is_empty());
    }
}
