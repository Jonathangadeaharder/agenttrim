use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use walkdir::WalkDir;

use crate::analyze::ledger_reader::{self, SkillUsageEntry};
use crate::analyze::safety_matrix::SafetyMatrix;
use crate::analyze::static_scanner;
use crate::time_provider::TimeProvider;
use agentalign_shared::models::{ReportKind, UnusedReport, UsageEntry};

/// Source of skill usage telemetry (SQLite + JSON ledgers).
pub trait UsageStore: Send + Sync {
    fn sqlite_usage(&self) -> Vec<UsageEntry>;
    fn json_usage(&self) -> HashMap<String, SkillUsageEntry>;
}

/// Production store backed by real SQLite + JSON ledger files.
pub struct RealUsageStore;

impl UsageStore for RealUsageStore {
    fn sqlite_usage(&self) -> Vec<UsageEntry> {
        ledger_reader::get_usage_stats().unwrap_or_default()
    }
    fn json_usage(&self) -> HashMap<String, SkillUsageEntry> {
        ledger_reader::read_skill_usage().unwrap_or_default()
    }
}

/// Test store with prerecorded data.
pub struct MockUsageStore {
    sqlite: Vec<UsageEntry>,
    json: HashMap<String, SkillUsageEntry>,
}

impl MockUsageStore {
    pub fn new(sqlite: Vec<UsageEntry>, json: HashMap<String, SkillUsageEntry>) -> Self {
        Self { sqlite, json }
    }
}

impl UsageStore for MockUsageStore {
    fn sqlite_usage(&self) -> Vec<UsageEntry> {
        self.sqlite.clone()
    }
    fn json_usage(&self) -> HashMap<String, SkillUsageEntry> {
        self.json.clone()
    }
}

/// Analyzes skill usage by cross-referencing installed skills
/// against the usage ledger and safety matrix.
pub struct SkillAnalyzer<'a> {
    pub time_provider: &'a dyn TimeProvider,
    pub usage_store: &'a dyn UsageStore,
}

impl<'a> SkillAnalyzer<'a> {
    pub fn new(time_provider: &'a dyn TimeProvider, usage_store: &'a dyn UsageStore) -> Self {
        Self { time_provider, usage_store }
    }

    /// Scan the agents skills directory and cross-reference with usage ledger.
    ///
    /// `agents_root` — path to `~/.agents/` or equivalent.
    /// `threshold_days` — number of days of inactivity before a skill is
    /// considered unused (default 90).
    /// `projects_root` — optional path for static text reference scanning.
    pub fn analyze(
        &self,
        agents_root: &Path,
        threshold_days: u64,
        projects_root: Option<&Path>,
    ) -> Result<Vec<UnusedReport>> {
        let skills_dir = agents_root.join("skills");
        if !skills_dir.exists() || !skills_dir.is_dir() {
            return Ok(Vec::new());
        }

        let sqlite_usage = self.usage_store.sqlite_usage();
        let json_usage = self.usage_store.json_usage();

        let now_secs = self.time_provider.now_secs();
        let threshold_secs = (threshold_days as i64) * 86_400;
        let cutoff = now_secs - threshold_secs;

        let mut reports = Vec::new();

        for entry in WalkDir::new(&skills_dir).min_depth(1).max_depth(1) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !entry.file_type().is_dir() && !entry.file_type().is_symlink() {
                continue;
            }

            let skill_name = entry
                .file_name()
                .to_str()
                .unwrap_or("")
                .to_string();

            if skill_name.is_empty() {
                continue;
            }

            let skill_path = entry.path().to_path_buf();

            if SafetyMatrix::is_protected(&skill_name) {
                let reason = SafetyMatrix::protection_reason(&skill_name)
                    .unwrap_or("Protected by safety matrix");
                reports.push(UnusedReport {
                    key_identifier: skill_name.clone(),
                    kind: ReportKind::Skill,
                    path_context: Some(skill_path.to_string_lossy().to_string()),
                    last_active_timestamp: None,
                    safe_to_purge: false,
                    reason: format!("Protected: {reason}"),
                });
                continue;
            }

            let usage_info = Self::get_usage_info(&skill_name, &sqlite_usage, &json_usage);
            let last_used = usage_info.last_used;
            let is_unused = last_used.map(|t| t < cutoff).unwrap_or(true);

            let file_exists = Self::skill_file_exists(&skill_path);

            let project_refs = match projects_root {
                Some(proot) => static_scanner::scan_for_references(
                    &skill_name,
                    &[proot],
                    &["md", "rs", "py", "ts", "js", "json", "toml", "yaml", "yml"],
                )
                .unwrap_or_default(),
                None => Vec::new(),
            };
            let has_project_refs = !project_refs.is_empty();

            let safe_to_purge = is_unused && !file_exists && !has_project_refs;

            let reason = if !file_exists && is_unused {
                "Skill directory exists but SKILL.md/binary not found; no recent usage"
            } else if is_unused && has_project_refs {
                "Skill appears unused in ledger but has project references"
            } else if is_unused {
                "No usage in telemetry within threshold period"
            } else {
                "Still actively used"
            };

            reports.push(UnusedReport {
                key_identifier: skill_name,
                kind: ReportKind::Skill,
                path_context: Some(skill_path.to_string_lossy().to_string()),
                last_active_timestamp: last_used,
                safe_to_purge,
                reason: reason.to_string(),
            });
        }

        Ok(reports)
    }

    /// Check if a skill is referenced in any project files (static grep).
    #[allow(dead_code)]
    pub fn check_project_references(
        skill_name: &str,
        projects_root: &Path,
    ) -> Result<bool> {
        let results = static_scanner::scan_for_references(
            skill_name,
            &[projects_root],
            &["md", "rs", "py", "ts", "js", "json", "toml", "yaml", "yml"],
        )?;
        Ok(!results.is_empty())
    }

    /// Check if the skill directory contains SKILL.md or binary files.
    fn skill_file_exists(skill_path: &Path) -> bool {
        if !skill_path.exists() {
            return false;
        }
        let target = if skill_path.is_symlink() {
            std::fs::read_link(skill_path).unwrap_or_else(|_| skill_path.to_path_buf())
        } else {
            skill_path.to_path_buf()
        };

        if !target.exists() {
            return false;
        }

        let indicators = ["SKILL.md", "skill.md", "main.py", "index.js", "index.ts"];
        indicators.iter().any(|name| target.join(name).exists())
    }

    /// Cross-reference a skill name against both SQLite and JSON usage ledgers.
    fn get_usage_info(
        skill_name: &str,
        sqlite_usage: &[UsageEntry],
        json_usage: &HashMap<String, SkillUsageEntry>,
    ) -> UsageInfo {
        let mut last_used: Option<i64> = None;
        let mut total_calls: u64 = 0;

        for entry in sqlite_usage {
            if entry.server_id == skill_name {
                if last_used
                    .map(|lu| entry.last_used_timestamp > lu)
                    .unwrap_or(true)
                {
                    last_used = Some(entry.last_used_timestamp);
                }
                total_calls = total_calls.saturating_add(entry.total_call_count);
            }
        }

        if let Some(json_entry) = json_usage.get(skill_name) {
            if let Some(ref last_str) = json_entry.last_used {
                if let Ok(ts) = last_str.parse::<i64>() {
                    if last_used.map(|lu| ts > lu).unwrap_or(true) {
                        last_used = Some(ts);
                    }
                }
            }
            total_calls = total_calls.saturating_add(json_entry.times_used);
        }

        UsageInfo { last_used, total_calls }
    }
}

/// Internal usage summary for a single skill.
struct UsageInfo {
    last_used: Option<i64>,
    total_calls: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_provider::FrozenTimeProvider;
    use std::fs;
    use std::path::PathBuf;

    fn create_skill_dir(base: &Path, name: &str) -> PathBuf {
        let dir = base.join("skills").join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_sqlite_entry(server_id: &str, ts: i64, calls: u64) -> UsageEntry {
        UsageEntry {
            server_id: server_id.to_string(),
            last_used_timestamp: ts,
            total_call_count: calls,
            context_window_byte_cost: None,
        }
    }

    fn make_mock_store(
        sqlite: Vec<UsageEntry>,
        json: Vec<(&str, i64, u64)>,
    ) -> MockUsageStore {
        let json_map: HashMap<String, SkillUsageEntry> = json
            .into_iter()
            .map(|(name, ts, calls)| {
                (
                    name.to_string(),
                    SkillUsageEntry {
                        last_used: Some(ts.to_string()),
                        times_used: calls,
                        ..Default::default()
                    },
                )
            })
            .collect();
        MockUsageStore::new(sqlite, json_map)
    }

    #[test]
    fn test_analyze_no_skills_dir() {
        let tmp = std::env::temp_dir().join("agenttrim_test_no_skills_refactor");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let store = MockUsageStore::new(vec![], HashMap::new());
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();
        assert!(reports.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_skill_file_exists_with_skil_md() {
        let tmp = std::env::temp_dir().join("agenttrim_test_skill_exists_v2");
        let _ = fs::remove_dir_all(&tmp);
        let dir = create_skill_dir(&tmp, "my-skill");
        fs::write(dir.join("SKILL.md"), "# My Skill\n").unwrap();

        assert!(SkillAnalyzer::skill_file_exists(&dir));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_skill_file_exists_empty_dir() {
        let tmp = std::env::temp_dir().join("agenttrim_test_empty_skill_v2");
        let _ = fs::remove_dir_all(&tmp);
        let dir = create_skill_dir(&tmp, "empty-skill");
        assert!(!SkillAnalyzer::skill_file_exists(&dir));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_skill_file_exists_broken_symlink() {
        let tmp = std::env::temp_dir().join("agenttrim_test_broken_link_v2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let skills_dir = tmp.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        let link = skills_dir.join("broken-skill");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/nonexistent/target", &link).unwrap();
        }
        #[cfg(windows)]
        {
            return;
        }
        assert!(!SkillAnalyzer::skill_file_exists(&link));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_check_project_references() {
        let tmp = std::env::temp_dir().join("agenttrim_test_proj_refs_v2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("config.md"), "Uses agent-browser for testing\n").unwrap();
        fs::write(tmp.join("main.rs"), "use agent_browser;\nfn main() {}\n").unwrap();

        let found = SkillAnalyzer::check_project_references("agent-browser", &tmp).unwrap();
        assert!(found);
        let found = SkillAnalyzer::check_project_references("nonexistent-skill", &tmp).unwrap();
        assert!(!found);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_unused_skill_becomes_candidate() {
        let tmp = std::env::temp_dir().join("agenttrim_test_unused_v2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("old-skill")).unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let store = MockUsageStore::new(vec![], HashMap::new());
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        let old = reports.iter().find(|r| r.key_identifier == "old-skill");
        assert!(old.is_some(), "old-skill should be analyzed");
        assert!(old.unwrap().safe_to_purge, "unused skill with no files should be purgeable");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_has_file_but_unused_not_purgeable() {
        let tmp = std::env::temp_dir().join("agenttrim_test_file_unused_v2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("present-skill")).unwrap();
        fs::write(tmp.join("skills").join("present-skill").join("SKILL.md"), "# Present\n").unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let store = MockUsageStore::new(vec![], HashMap::new());
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        let present = reports.iter().find(|r| r.key_identifier == "present-skill");
        assert!(present.is_some());
        assert!(!present.unwrap().safe_to_purge, "skill with existing SKILL.md should not be purgeable");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_protected_skill_not_purgeable() {
        let tmp = std::env::temp_dir().join("agenttrim_test_protected_v2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("agent-browser")).unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let store = MockUsageStore::new(vec![], HashMap::new());
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        let protected = reports.iter().find(|r| r.key_identifier == "agent-browser");
        assert!(protected.is_some());
        assert!(!protected.unwrap().safe_to_purge);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_used_skill_not_purgeable() {
        let tmp = std::env::temp_dir().join("agenttrim_test_used_v2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("active-skill")).unwrap();
        fs::write(tmp.join("skills").join("active-skill").join("SKILL.md"), "# Active\n").unwrap();

        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        let recent_ts = now - 1;
        let store = make_mock_store(
            vec![make_sqlite_entry("active-skill", recent_ts, 5)],
            vec![],
        );
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        let active = reports.iter().find(|r| r.key_identifier == "active-skill");
        assert!(active.is_some());
        assert!(!active.unwrap().safe_to_purge, "recently used skill should not be purgeable");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_old_usage_kept_for_cutoff() {
        let tmp = std::env::temp_dir().join("agenttrim_test_old_usage_v2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("oldy")).unwrap();

        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        let cutoff = now - (90 * 86_400);
        let old_ts = cutoff - 1;
        let store = make_mock_store(
            vec![make_sqlite_entry("oldy", old_ts, 1)],
            vec![],
        );
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        let old = reports.iter().find(|r| r.key_identifier == "oldy");
        assert!(old.is_some());
        assert!(old.unwrap().safe_to_purge, "skill with old usage should be purgeable");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_skips_regular_files_but_analyzes_dirs() {
        let tmp = std::env::temp_dir().join("agenttrim_test_mixed_entries");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("real-skill")).unwrap();
        fs::write(tmp.join("skills").join("README.md"), b"not a skill").unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let store = MockUsageStore::new(vec![], HashMap::new());
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();
        assert_eq!(reports.len(), 1, "only the dir should produce a report");
        assert_eq!(reports[0].key_identifier, "real-skill");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_boundary_timestamp_exact_cutoff() {
        let tmp = std::env::temp_dir().join("agenttrim_test_boundary_cutoff");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("boundary-skill")).unwrap();

        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        let cutoff = now - (90 * 86_400);
        let store = make_mock_store(
            vec![make_sqlite_entry("boundary-skill", cutoff, 1)],
            vec![],
        );
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        // is_unused = last_used.map(|t| t < cutoff).unwrap_or(true)
        // t == cutoff → t < cutoff is false → is_unused = false → NOT purgeable
        let b = reports.iter().find(|r| r.key_identifier == "boundary-skill");
        assert!(b.is_some());
        assert!(!b.unwrap().safe_to_purge, "ts == cutoff is NOT less than cutoff, skill still used");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_non_matching_sqlite_entries_skipped() {
        let tmp = std::env::temp_dir().join("agenttrim_test_non_match");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("target")).unwrap();

        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        let store = make_mock_store(
            vec![
                make_sqlite_entry("other-skill", now - 1, 5),
                make_sqlite_entry("another-skill", now - 1, 3),
            ],
            vec![],
        );
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        let t = reports.iter().find(|r| r.key_identifier == "target");
        assert!(t.is_some());
        assert!(t.unwrap().safe_to_purge, "non-matching sqlite entries should not affect target");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_project_refs_block_purge() {
        let tmp = std::env::temp_dir().join("agenttrim_test_proj_block");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("refd-skill")).unwrap();
        let proj = tmp.join("projects");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("config.md"), "refd-skill used here\n").unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let store = MockUsageStore::new(vec![], HashMap::new());
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, Some(&proj)).unwrap();

        let r = reports.iter().find(|r| r.key_identifier == "refd-skill");
        assert!(r.is_some());
        assert!(!r.unwrap().safe_to_purge, "project-refd skill should not be purgeable");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_skills_dir_is_file_not_dir() {
        let tmp = std::env::temp_dir().join("agenttrim_test_file_not_dir");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("skills"), b"this is a file not a dir").unwrap();

        let tp = FrozenTimeProvider(1_000_000_000);
        let store = MockUsageStore::new(vec![], HashMap::new());
        let analyzer = SkillAnalyzer::new(&tp, &store);
        // Original: !exists || !is_dir → !true || !false = false || true = true → returns empty
        // Mutated: !exists && !is_dir → !true && !false = false && true = false → proceeds
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();
        assert!(reports.is_empty(), "file-based skills dir should produce no reports");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_two_sqlite_entries_picks_latest() {
        let tmp = std::env::temp_dir().join("agenttrim_test_two_sqlite");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("dup-skill")).unwrap();

        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        let store = make_mock_store(
            vec![
                make_sqlite_entry("dup-skill", 100, 1),
                make_sqlite_entry("dup-skill", 900_000_000, 5),
            ],
            vec![],
        );
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        let d = reports.iter().find(|r| r.key_identifier == "dup-skill");
        assert!(d.is_some());
        // cutoff = now - 90*86400 = 1_000_000_000 - 7_776_000 = 992_224_000
        // second entry has ts = 900_000_000 which is < cutoff, so is_unused = true
        // But first entry (ts = 100) is even older
        // With > mutated to ==, ts 100 would be used (wrong)
        assert_eq!(d.unwrap().last_active_timestamp, Some(900_000_000), "should pick latest ts");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_analyze_mixed_usage_and_json_overrides() {
        let tmp = std::env::temp_dir().join("agenttrim_test_mixed_v2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("skills").join("mixed")).unwrap();

        let now = 1_000_000_000;
        let tp = FrozenTimeProvider(now);
        // SQLite says old (100), JSON says recent (now-1) — JSON should win (no SKILL.md to mask)
        let store = make_mock_store(
            vec![make_sqlite_entry("mixed", 100, 1)],
            vec![("mixed", now - 1, 10)],
        );
        let analyzer = SkillAnalyzer::new(&tp, &store);
        let reports = analyzer.analyze(&tmp, 90, None).unwrap();

        let mixed = reports.iter().find(|r| r.key_identifier == "mixed");
        assert!(mixed.is_some());
        // JSON recent should take precedence: last_active_timestamp should be from JSON (now-1)
        // With > mutated to == on line 224, JSON ts would NOT override, last_used stays at 100
        assert_eq!(mixed.unwrap().last_active_timestamp, Some(now - 1), "JSON recent should override SQLite old");
        assert!(!mixed.unwrap().safe_to_purge, "JSON recent usage should make skill not purgeable");
        let _ = fs::remove_dir_all(&tmp);
    }
}
