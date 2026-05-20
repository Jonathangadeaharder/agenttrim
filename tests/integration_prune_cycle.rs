use std::fs;
use tempfile::TempDir;

use agenttrim::analyze::skills::{RealUsageStore, SkillAnalyzer};
use agenttrim::time_provider::SystemTimeProvider;

fn create_empty_skill_dir(skills_dir: &std::path::Path, name: &str) {
    let dir = skills_dir.join(name);
    fs::create_dir_all(&dir).unwrap();
}

fn create_active_skill_dir(skills_dir: &std::path::Path, name: &str) {
    let dir = skills_dir.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), format!("# {}\n\nActive skill", name)).unwrap();
}

fn make_analyzer() -> SkillAnalyzer<'static> {
    // Static lifetime works because these are singletons with no state
    static TP: SystemTimeProvider = SystemTimeProvider;
    static STORE: RealUsageStore = RealUsageStore;
    SkillAnalyzer::new(&TP, &STORE)
}

#[test]
fn test_skill_analysis_stale_skill_is_safe_to_purge() {
    let sandbox = TempDir::new().unwrap();
    let agents_root = sandbox.path().join(".agents");
    let skills_dir = agents_root.join("skills");
    fs::create_dir_all(&skills_dir).unwrap();

    create_empty_skill_dir(&skills_dir, "stale-assistant");
    create_active_skill_dir(&skills_dir, "active-tool");

    let analyzer = make_analyzer();
    let reports = analyzer.analyze(&agents_root, 0, None).expect("Analysis should succeed");

    let stale_report = reports.iter()
        .find(|r| r.key_identifier == "stale-assistant")
        .expect("Should find stale-assistant");

    assert!(stale_report.safe_to_purge, "Stale skill with no SKILL.md should be safe to purge");
    assert_eq!(stale_report.kind, agentalign_shared::models::ReportKind::Skill);

    let active_report = reports.iter()
        .find(|r| r.key_identifier == "active-tool")
        .expect("Should find active-tool");

    assert!(!active_report.safe_to_purge, "Active skill with SKILL.md should NOT be safe to purge");
}

#[test]
fn test_prune_removes_stale_skills() {
    let sandbox = TempDir::new().unwrap();
    let agents_root = sandbox.path().join(".agents");
    let skills_dir = agents_root.join("skills");
    fs::create_dir_all(&skills_dir).unwrap();

    create_empty_skill_dir(&skills_dir, "stale-to-remove");

    let analyzer = make_analyzer();
    let reports = analyzer.analyze(&agents_root, 0, None).expect("Analysis should succeed");

    let stale: Vec<agentalign_shared::models::UnusedReport> = reports.into_iter().filter(|r| r.safe_to_purge).collect();
    assert!(!stale.is_empty(), "Should find stale skills");

    let outcome = agenttrim::prune::prune_skills_unified(&stale, &skills_dir, false)
        .expect("Prune should succeed");

    assert!(outcome.removed.contains(&"stale-to-remove".to_string()));
    assert!(!skills_dir.join("stale-to-remove").exists(), "Stale skill should be removed");
}

#[test]
fn test_analyze_empty_skills_dir() {
    let sandbox = TempDir::new().unwrap();
    let agents_root = sandbox.path().join(".agents");
    let skills_dir = agents_root.join("skills");
    fs::create_dir_all(&skills_dir).unwrap();

    let analyzer = make_analyzer();
    let reports = analyzer.analyze(&agents_root, 90, None).expect("Analysis should succeed");

    assert!(reports.is_empty(), "Empty skills dir should produce no reports");
}

#[test]
fn test_analyze_no_skills_dir() {
    let sandbox = TempDir::new().unwrap();
    let agents_root = sandbox.path().join(".agents");

    let analyzer = make_analyzer();
    let reports = analyzer.analyze(&agents_root, 90, None).expect("Analysis should succeed with missing dir");

    assert!(reports.is_empty(), "Missing skills dir should produce no reports");
}

#[test]
fn test_prune_dry_run_does_not_delete() {
    let sandbox = TempDir::new().unwrap();
    let agents_root = sandbox.path().join(".agents");
    let skills_dir = agents_root.join("skills");
    fs::create_dir_all(&skills_dir).unwrap();

    create_empty_skill_dir(&skills_dir, "dry-run-test");

    let analyzer = make_analyzer();
    let reports = analyzer.analyze(&agents_root, 0, None).expect("Analysis should succeed");

    let stale: Vec<agentalign_shared::models::UnusedReport> = reports.into_iter().filter(|r| r.safe_to_purge).collect();

    let outcome = agenttrim::prune::prune_skills_unified(&stale, &skills_dir, true)
        .expect("Dry-run prune should succeed");

    assert!(outcome.removed.contains(&"dry-run-test".to_string()));
    assert!(skills_dir.join("dry-run-test").exists(), "Dry run should not delete");
}
