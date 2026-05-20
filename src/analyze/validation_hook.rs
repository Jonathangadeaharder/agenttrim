use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::analyze::safety_matrix::SafetyMatrix;
use crate::time_provider::TimeProvider;
use agentalign_shared::models::UnusedReport;

pub const SEVEN_DAYS_SECS: i64 = 7 * 86_400;
pub const NINETY_DAYS_SECS: i64 = 90 * 86_400;

/// Severity level of a validation issue.
#[derive(Debug, Clone, PartialEq)]
pub enum IssueSeverity {
    Block,
    Warn,
}

/// A single validation issue found during pre-purge checks.
#[derive(Debug, Clone)]
pub struct ValidationIssue {
    pub item: String,
    pub severity: IssueSeverity,
    pub message: String,
}

/// Pre-purge integrity check engine with injected time provider.
pub struct PrePurgeValidation<'a> {
    pub time_provider: &'a dyn TimeProvider,
}

impl<'a> PrePurgeValidation<'a> {
    pub fn new(time_provider: &'a dyn TimeProvider) -> Self {
        Self { time_provider }
    }

    pub fn validate(&self, reports: &[UnusedReport]) -> Result<Vec<ValidationIssue>> {
        let mut issues = Vec::new();

        for report in reports {
            if !report.safe_to_purge {
                continue;
            }

            if SafetyMatrix::is_protected(&report.key_identifier) {
                let reason = SafetyMatrix::protection_reason(&report.key_identifier)
                    .unwrap_or("Protected by safety matrix");
                issues.push(ValidationIssue {
                    item: report.key_identifier.clone(),
                    severity: IssueSeverity::Block,
                    message: format!("Item is protected: {reason}"),
                });
            }

            if let Some(ref ctx) = report.path_context {
                let config_path = Path::new(ctx);
                if config_path.exists() {
                    issues.push(ValidationIssue {
                        item: report.key_identifier.clone(),
                        severity: IssueSeverity::Warn,
                        message: format!(
                            "Path still exists at {}; verify it's not actively referenced",
                            ctx
                        ),
                    });
                }
            }

            let backup_dir = Self::backup_dir();
            match Self::check_backup_writable(&backup_dir) {
                Ok(_) => {}
                Err(e) => {
                    issues.push(ValidationIssue {
                        item: report.key_identifier.clone(),
                        severity: IssueSeverity::Warn,
                        message: format!("Backup path may not be writable: {e}"),
                    });
                }
            }

            if let Some(ref ctx) = report.path_context {
                let item_path = Path::new(ctx);
                if item_path.exists() {
                    if let Ok(metadata) = item_path.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            if let Ok(elapsed) = modified.elapsed() {
                                if elapsed.as_secs() < SEVEN_DAYS_SECS as u64 {
                                    issues.push(ValidationIssue {
                                        item: report.key_identifier.clone(),
                                        severity: IssueSeverity::Warn,
                                        message: format!(
                                            "File modified less than 7 days ago at {}",
                                            ctx
                                        ),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(issues)
    }

    pub fn validate_candidates(&self, reports: &[UnusedReport]) -> Result<Vec<UnusedReport>> {
        let now = self.time_provider.now_secs();
        let cutoff = now - NINETY_DAYS_SECS;

        let validated: Vec<UnusedReport> = reports
            .iter()
            .filter(|r| {
                if !r.safe_to_purge {
                    return false;
                }

                if SafetyMatrix::is_protected(&r.key_identifier) {
                    return false;
                }

                if let Some(ref ctx) = r.path_context {
                    let item_path = Path::new(ctx);
                    if item_path.exists() {
                        if let Ok(metadata) = item_path.metadata() {
                            if let Ok(modified) = metadata.modified() {
                                if let Ok(elapsed) = modified.elapsed() {
                                    if elapsed.as_secs() < SEVEN_DAYS_SECS as u64 {
                                        return false;
                                    }
                                }
                            }
                        }
                    }
                }

                match r.last_active_timestamp {
                    Some(ts) => ts < cutoff,
                    None => true,
                }
            })
            .cloned()
            .collect();

        Ok(validated)
    }

    pub fn safety_gate(&self, report: &UnusedReport) -> Result<()> {
        if SafetyMatrix::is_protected(&report.key_identifier) {
            anyhow::bail!(
                "Safety gate: {} is protected and cannot be pruned",
                report.key_identifier
            );
        }

        if !report.safe_to_purge {
            anyhow::bail!(
                "Safety gate: {} is not marked as safe to purge. Reason: {}",
                report.key_identifier,
                report.reason
            );
        }

        if let Some(ref ctx) = report.path_context {
            let item_path = Path::new(ctx);
            if item_path.exists() {
                if let Ok(metadata) = item_path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(elapsed) = modified.elapsed() {
                            if elapsed.as_secs() < SEVEN_DAYS_SECS as u64 {
                                anyhow::bail!(
                                    "Safety gate: {} was modified less than 7 days ago at {}",
                                    report.key_identifier,
                                    ctx
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn backup_dir() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(".agents").join("backups"))
            .unwrap_or_else(|| PathBuf::from("/tmp/agenttrim-backups"))
    }

    fn check_backup_writable(path: &Path) -> Result<()> {
        if path.exists() {
            if !path.is_dir() {
                anyhow::bail!("Backup path exists but is not a directory: {:?}", path);
            }
            let test_file = path.join(".agenttrim_write_test");
            std::fs::write(&test_file, b"test")?;
            std::fs::remove_file(&test_file)?;
        } else {
            std::fs::create_dir_all(path)
                .with_context(|| format!("Cannot create backup directory: {:?}", path))?;
            let _ = std::fs::remove_dir(path);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_provider::FrozenTimeProvider;
    use agentalign_shared::models::ReportKind;
    use std::fs;

    fn make_report(id: &str, path: Option<&str>, safe: bool, ts: Option<i64>) -> UnusedReport {
        UnusedReport {
            key_identifier: id.to_string(),
            kind: ReportKind::Skill,
            path_context: path.map(|s| s.to_string()),
            last_active_timestamp: ts,
            safe_to_purge: safe,
            reason: "Test candidate".to_string(),
        }
    }

    // --- validate_candidates tests ---

    #[test]
    fn test_validate_candidates_filters_non_candidates() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let reports = vec![make_report("s1", None, false, None)];
        let result = v.validate_candidates(&reports).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_validate_candidates_keeps_safe_purge() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let reports = vec![make_report("s1", None, true, None)];
        let result = v.validate_candidates(&reports).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_validate_candidates_filters_protected() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let reports = vec![make_report("supabase", None, true, None)];
        let result = v.validate_candidates(&reports).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_validate_candidates_recent_timestamp_filtered() {
        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        let v = PrePurgeValidation::new(&tp);
        let recent_ts = now - 1;
        let reports = vec![make_report("s1", None, true, Some(recent_ts))];
        let result = v.validate_candidates(&reports).unwrap();
        assert!(result.is_empty(), "recent timestamp should be filtered");
    }

    #[test]
    fn test_validate_candidates_old_timestamp_kept() {
        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        let v = PrePurgeValidation::new(&tp);
        let old_ts = now - NINETY_DAYS_SECS - 1;
        let reports = vec![make_report("s1", None, true, Some(old_ts))];
        let result = v.validate_candidates(&reports).unwrap();
        assert_eq!(result.len(), 1, "old timestamp should pass");
    }

    #[test]
    fn test_validate_candidates_boundary_timestamp() {
        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        let v = PrePurgeValidation::new(&tp);
        let boundary = now - NINETY_DAYS_SECS;
        let reports = vec![make_report("s1", None, true, Some(boundary))];
        let result = v.validate_candidates(&reports).unwrap();
        assert!(
            result.is_empty(),
            "exactly at cutoff should be filtered (ts < cutoff, not <=)"
        );
    }

    #[test]
    fn test_validate_candidates_none_timestamp_kept() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let reports = vec![make_report("s1", None, true, None)];
        let result = v.validate_candidates(&reports).unwrap();
        assert_eq!(result.len(), 1, "no recorded usage should pass");
    }

    #[test]
    fn test_validate_candidates_mtime_blocks_fresh_file() {
        let tmp = std::env::temp_dir().join("agenttrim_test_vc_mtime");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("fresh.txt"), b"x").unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let path_str = tmp.join("fresh.txt").to_string_lossy().to_string();
        let reports = vec![make_report("fresh", Some(&path_str), true, Some(1))];
        let result = v.validate_candidates(&reports).unwrap();
        assert!(result.is_empty(), "fresh file should be blocked by mtime check");

        let _ = fs::remove_dir_all(&tmp);
    }

    // --- safety_gate tests ---

    #[test]
    fn test_safety_gate_protected() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let report = make_report("agent-browser", Some("/tmp/test"), true, None);
        let result = v.safety_gate(&report);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("protected"));
    }

    #[test]
    fn test_safety_gate_not_safe() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let report = make_report("some-skill", Some("/tmp/test"), false, None);
        let result = v.safety_gate(&report);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not marked as safe"));
    }

    #[test]
    fn test_safety_gate_ok_without_path() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let report = make_report("clean-skill", None, true, None);
        let result = v.safety_gate(&report);
        assert!(result.is_ok());
    }

    #[test]
    fn test_safety_gate_rejects_recently_modified() {
        let tmp = std::env::temp_dir().join("agenttrim_test_safety_recent");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("fresh.md"), b"x").unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let path_str = tmp.to_string_lossy().to_string();
        let report = make_report("fresh-skill", Some(&path_str), true, None);
        let result = v.safety_gate(&report);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("less than 7 days"));

        let _ = fs::remove_dir_all(&tmp);
    }

    // --- validate tests ---

    #[test]
    fn test_validate_skips_non_candidates() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let reports = vec![make_report("skill-a", None, false, None)];
        let issues = v.validate(&reports).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn test_validate_protected_candidate() {
        let tp = FrozenTimeProvider(1_000_000_000);
        let v = PrePurgeValidation::new(&tp);
        let report = make_report("supabase", None, true, None);
        let issues = v.validate(&[report]).unwrap();
        assert!(issues.iter().any(|i| i.severity == IssueSeverity::Block));
    }
}
