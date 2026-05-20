/// Hardcoded never-prune allowlist.
///
/// Items matched by exact ID or by glob pattern (`postgres-*`, `sequential-*`)
/// are protected and cannot be pruned by agenttrim's safety gates.
pub struct SafetyMatrix;

/// Exact-match protected IDs and their protection reason.
const PROTECTED_ENTRIES: &[(&str, &str)] = &[
    (
        "agent-browser",
        "Critical browser automation infrastructure",
    ),
    ("find-skills", "Skill discovery system"),
    ("supabase", "Persistent cloud service config"),
    ("test-review", "Mandatory CI quality gate"),
    ("filesystem", "Core filesystem access tool"),
];

/// Pattern-based protected IDs (globs with `-*` suffix) and their reasons.
const PROTECTED_PATTERNS: &[(&str, &str)] = &[
    ("postgres-*", "Database executor (pattern)"),
    ("sequential-*", "Sequential thinking tools (pattern)"),
];

/// Static list of exact-match protected IDs only (no patterns).
#[allow(dead_code)]
const PROTECTED_EXACT_IDS: &[&str] = &[
    "agent-browser",
    "find-skills",
    "supabase",
    "test-review",
    "filesystem",
];

impl SafetyMatrix {
    /// Returns true if this skill/server is protected and cannot be pruned.
    pub fn is_protected(id: &str) -> bool {
        PROTECTED_ENTRIES.iter().any(|&(pid, _)| pid == id) || Self::matches_protected_pattern(id)
    }

    /// Returns the reason why an item is protected.
    pub fn protection_reason(id: &str) -> Option<&'static str> {
        PROTECTED_ENTRIES
            .iter()
            .find(|&&(pid, _)| pid == id)
            .map(|&(_, reason)| reason)
            .or_else(|| Self::pattern_protection_reason(id))
    }

    /// Returns all exact-match protected IDs.
    #[allow(dead_code)]
    pub fn protected_ids() -> &'static [&'static str] {
        PROTECTED_EXACT_IDS
    }

    /// Checks if a server ID matches a protected pattern (glob/regex).
    ///
    /// Currently supports `prefix-*` glob patterns. Patterns are matched
    /// as prefix checks: `postgres-*` matches `postgres-main`, `postgres-analytics`, etc.
    pub fn matches_protected_pattern(id: &str) -> bool {
        PROTECTED_PATTERNS.iter().any(|(pattern, _)| {
            if let Some(prefix) = pattern.strip_suffix('*') {
                id.starts_with(prefix)
            } else {
                id == *pattern
            }
        })
    }

    /// Returns the reason for a pattern-based protection match.
    fn pattern_protection_reason(id: &str) -> Option<&'static str> {
        PROTECTED_PATTERNS.iter().find_map(|(pattern, reason)| {
            let prefix = pattern.strip_suffix('*')?;
            if id.starts_with(prefix) {
                Some(*reason)
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_protected() {
        assert!(SafetyMatrix::is_protected("agent-browser"));
        assert!(SafetyMatrix::is_protected("supabase"));
        assert!(!SafetyMatrix::is_protected("unknown-tool"));
    }

    #[test]
    fn test_pattern_protected() {
        assert!(SafetyMatrix::is_protected("postgres-main"));
        assert!(SafetyMatrix::is_protected("postgres-analytics"));
        assert!(SafetyMatrix::is_protected("sequential-thinker"));
        assert!(!SafetyMatrix::is_protected("postgresql-extra"));
    }

    #[test]
    fn test_protection_reason() {
        let reason = SafetyMatrix::protection_reason("agent-browser");
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("browser"));

        let reason = SafetyMatrix::protection_reason("postgres-main");
        assert!(reason.is_some());
        assert_eq!(reason.unwrap(), "Database executor (pattern)");

        let reason = SafetyMatrix::protection_reason("unknown");
        assert!(reason.is_none());
    }

    #[test]
    fn test_protected_ids_contains_exact() {
        let ids = SafetyMatrix::protected_ids();
        assert!(ids.contains(&"agent-browser"));
        assert!(ids.contains(&"supabase"));
        assert!(!ids.contains(&"postgres-main")); // patterns not in exact list
    }

    #[test]
    fn test_not_protected() {
        assert!(!SafetyMatrix::is_protected("random-thing"));
        assert!(!SafetyMatrix::is_protected(""));
        assert!(!SafetyMatrix::is_protected("postgres")); // no dash, doesn't match postgres-*
    }
}
