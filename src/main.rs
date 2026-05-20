use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;

use agenttrim::analyze;
use agenttrim::analyze::ledger_reader;
use agenttrim::prune;
use agenttrim::time_provider::SystemTimeProvider;
use agentalign_shared::models::McpServerDefinition;

use analyze::validation_hook::{IssueSeverity, PrePurgeValidation};

#[derive(Parser)]
#[command(name = "agenttrim", about = "Telemetry-Driven Pruning & Vacuum Engine")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan and report unused resources
    Analyze {
        /// Path to ~/.agents/ (default: home dir)
        #[arg(long)]
        agents_root: Option<PathBuf>,

        /// Path to canonical MCP config (default: ~/.agents/mcp_config.json)
        #[arg(long)]
        mcp_config: Option<PathBuf>,

        /// Optional projects root for static reference scanning
        #[arg(long)]
        projects_root: Option<PathBuf>,

        /// Inactivity threshold in days (default: 90)
        #[arg(long, default_value_t = 90)]
        threshold_days: u64,
    },
    /// Remove unused resources (with safety gates)
    Prune {
        /// Path to ~/.agents/ (default: home dir)
        #[arg(long)]
        agents_root: Option<PathBuf>,

        /// Path to canonical MCP config (default: ~/.agents/mcp_config.json)
        #[arg(long)]
        mcp_config: Option<PathBuf>,

        /// Non-interactive mode (still passes safety gates)
        #[arg(long)]
        force: bool,

        /// Show what would be pruned without deleting
        #[arg(long)]
        dry_run: bool,
    },
    /// Deep clean: zombie processes, orphaned caches
    Vacuum {
        /// Show what would be killed without actually killing
        #[arg(long)]
        dry_run: bool,
    },
    /// Show usage stats from SQLite ledger
    Status,
    /// Daemon: watch skill/MCP filesystem for access, log usage to SQLite
    Watch {
        /// Poll interval in seconds (default: 60)
        #[arg(long, default_value_t = 60)]
        interval: u64,
    },
    /// Configure thresholds and allowlists
    Config,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Analyze {
            agents_root,
            mcp_config,
            projects_root,
            threshold_days,
        } => run_analyze(agents_root, mcp_config, projects_root, threshold_days),
        Commands::Prune {
            agents_root,
            mcp_config,
            force,
            dry_run,
        } => run_prune(agents_root, mcp_config, force, dry_run),
        Commands::Vacuum { dry_run } => {
            run_vacuum(dry_run);
        }
        Commands::Status => {
            run_status();
        }
        Commands::Watch { interval } => {
            run_watch(interval);
        }
        Commands::Config => {
            println!("⚙️  config — configure thresholds and allowlists");
            // TODO: implement config subcommand
        }
    }
}

fn resolve_agents_root(custom: Option<PathBuf>) -> PathBuf {
    custom.unwrap_or_else(|| {
        dirs::home_dir()
            .expect("Cannot determine home directory")
            .join(".agents")
    })
}

fn resolve_mcp_config(custom: Option<PathBuf>) -> PathBuf {
    custom.unwrap_or_else(|| {
        dirs::home_dir()
            .expect("Cannot determine home directory")
            .join(".agents")
            .join("mcp_config.json")
    })
}

fn run_analyze(
    agents_root_opt: Option<PathBuf>,
    mcp_config_opt: Option<PathBuf>,
    projects_root_opt: Option<PathBuf>,
    threshold_days: u64,
) {
    let agents_root = resolve_agents_root(agents_root_opt);
    let mcp_config = resolve_mcp_config(mcp_config_opt);
    let projects_root = projects_root_opt.as_deref();

    println!("🔍 analyze — scanning for unused skills, MCP servers, and processes...");
    println!("  agents root:  {}", agents_root.display());
    println!("  mcp config:   {}", mcp_config.display());
    if let Some(pr) = projects_root {
        println!("  projects:     {}", pr.display());
    }
    println!("  threshold:    {threshold_days} days");

    let tp = SystemTimeProvider;
    match crate::analyze::run_full_analysis(
        &tp,
        &agents_root,
        &mcp_config,
        projects_root,
        threshold_days,
    ) {
        Ok(report) => {
            println!();
            println!("=== Analysis Report ===");
            println!();
            println!("Skills:");
            for skill in &report.skills {
                let status = if skill.safe_to_purge {
                    "CANDIDATE"
                } else {
                    "KEEP"
                };
                println!(
                    "  [{status}] {} — {}",
                    skill.key_identifier, skill.reason
                );
                if let Some(ref path) = skill.path_context {
                    println!("         path: {path}");
                }
            }

            println!();
            println!("MCP Servers:");
            for server in &report.mcp_servers {
                let status = if server.safe_to_purge {
                    "CANDIDATE"
                } else {
                    "KEEP"
                };
                println!(
                    "  [{status}] {} — {}",
                    server.key_identifier, server.reason
                );
            }

            println!();
            println!("Running Processes:");
            for proc in &report.processes {
                let orphan = if proc.is_orphan { " (orphan)" } else { "" };
                println!("  PID {} — {}{orphan}", proc.pid, proc.command);
                if let Some(ref matched) = proc.matched_server {
                    println!("         matched: {matched}");
                }
            }

            println!();
            println!("=== Summary ===");
            println!("  Total candidates:        {}", report.total_candidates);
            println!("  Protected/ignored:       {}", report.protected_ignored);
            println!("  Skills analyzed:         {}", report.skills.len());
            println!("  MCP servers analyzed:   {}", report.mcp_servers.len());
            println!("  Running processes found: {}", report.processes.len());
        }
        Err(e) => {
            eprintln!("  ✗ Analysis failed: {e}");
        }
    }
}

fn run_prune(
    agents_root_opt: Option<PathBuf>,
    mcp_config_opt: Option<PathBuf>,
    force: bool,
    dry_run: bool,
) {
    let agents_root = resolve_agents_root(agents_root_opt);
    let mcp_config = resolve_mcp_config(mcp_config_opt);

    println!("🧹 prune — cleaning unused resources (safety gates enabled)");
    if dry_run {
        println!("  → DRY RUN: no changes will be made");
    }
    if force {
        println!("  → FORCE: non-interactive mode");
    }

    // Run analysis first
    let projects_root = None;
    let threshold_days: u64 = 90;
    let tp = SystemTimeProvider;

    let report = match crate::analyze::run_full_analysis(
        &tp,
        &agents_root,
        &mcp_config,
        projects_root,
        threshold_days,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  ✗ Analysis failed: {e}");
            return;
        }
    };

    if report.total_candidates == 0 {
        println!("  ✓ No candidates found for pruning.");
        return;
    }

    println!();
    println!("=== Prune Candidates ===");

    // Collect all candidates
    let all_reports: Vec<_> = report
        .skills
        .iter()
        .chain(report.mcp_servers.iter())
        .filter(|r| r.safe_to_purge)
        .collect();

    for r in &all_reports {
        let kind = match r.kind {
            agentalign_shared::models::ReportKind::Skill => "skill",
            agentalign_shared::models::ReportKind::McpServer => "mcp",
            agentalign_shared::models::ReportKind::OrphanedProcess => "process",
            agentalign_shared::models::ReportKind::StaleBackup => "backup",
        };
        println!(
            "  [{kind}] {} — {}",
            r.key_identifier, r.reason
        );
        if let Some(ref path) = r.path_context {
            println!("         path: {path}");
        }
    }

    println!();
    println!("{} item(s) to prune.", all_reports.len());

    // Run validation hooks
    println!();
    println!("=== Validation ===");

    let validator = PrePurgeValidation::new(&tp);
    match validator.validate(
        &all_reports.iter().map(|r| (*r).clone()).collect::<Vec<_>>(),
    ) {
        Ok(issues) => {
            if issues.is_empty() {
                println!("  ✓ No validation issues.");
            } else {
                for issue in &issues {
                    let severity = match issue.severity {
                        IssueSeverity::Block => "BLOCK",
                        IssueSeverity::Warn => "WARN",
                    };
                    println!("  [{severity}] {} — {}", issue.item, issue.message);
                }

                let blocked = issues.iter().any(|i| i.severity == IssueSeverity::Block);
                if blocked {
                    eprintln!("  ✗ Blocking issues found. Resolve before pruning.");
                    return;
                }
            }
        }
        Err(e) => {
            eprintln!("  ✗ Validation error: {e}");
            return;
        }
    }

    // Interactive confirmation (unless force or dry-run)
    if !force && !dry_run {
        println!();
        println!("Proceed with pruning? (yes/no)");
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            eprintln!("  ✗ Failed to read input. Use --force to skip confirmation.");
            return;
        }
        let trimmed = input.trim().to_lowercase();
        if trimmed != "yes" && trimmed != "y" {
            println!("  ✗ Aborted by user.");
            return;
        }
    }

    // Execute pruning
    println!();
    println!("=== Executing Prune ===");

    // Skills pruning
    let skills_reports: Vec<_> = report.skills.iter().filter(|r| r.safe_to_purge).cloned().collect();
    if !skills_reports.is_empty() {
        let skills_root = agents_root.join("skills");
        match crate::prune::prune_skills_unified(&skills_reports, &skills_root, dry_run) {
            Ok(pr) => {
                for removed in &pr.removed {
                    if dry_run {
                        println!("  [DRY_RUN] would remove skill: {removed}");
                    } else {
                        println!("  [REMOVED] skill: {removed}");
                    }
                }
                for skipped in &pr.skipped_protected {
                    println!("  [SKIPPED] protected skill: {skipped}");
                }
                for (id, err) in &pr.skipped_error {
                    eprintln!("  [ERROR] {id}: {err}");
                }
                if let Some(bp) = pr.backup_path {
                    println!("  Backup saved to: {}", bp.display());
                }
            }
            Err(e) => {
                eprintln!("  ✗ Skills pruning failed: {e}");
            }
        }
    }

    // MCP pruning
    let mcp_reports: Vec<_> = report
        .mcp_servers
        .iter()
        .filter(|r| r.safe_to_purge)
        .cloned()
        .collect();
    if !mcp_reports.is_empty() {
        match crate::prune::prune_mcp_unified(&mcp_reports, &mcp_config, dry_run) {
            Ok(pr) => {
                for removed in &pr.removed {
                    if dry_run {
                        println!("  [DRY_RUN] would remove MCP server: {removed}");
                    } else {
                        println!("  [REMOVED] MCP server: {removed}");
                    }
                }
                for skipped in &pr.skipped_protected {
                    println!("  [SKIPPED] protected MCP server: {skipped}");
                }
                for (id, err) in &pr.skipped_error {
                    eprintln!("  [ERROR] {id}: {err}");
                }
                if let Some(bp) = pr.backup_path {
                    println!("  Backup saved to: {}", bp.display());
                }
            }
            Err(e) => {
                eprintln!("  ✗ MCP pruning failed: {e}");
            }
        }
    }

    println!("  ✓ Prune complete.");
}

fn run_status() {
    match ledger_reader::get_usage_stats() {
        Ok(entries) => {
            if entries.is_empty() {
                println!("No usage records found. The `agenttrim watch` daemon will populate this automatically.");
            } else {
                println!("{:<22} {:>8} {:>12}", "Server", "Calls", "Last Used");
                println!("{}", "-".repeat(46));
                for e in &entries {
                    println!("{:<22} {:>8} {:>12}", e.server_id, e.total_call_count, e.last_used_timestamp);
                }
            }
        }
        Err(e) => eprintln!("Error reading usage: {}", e),
    }

    match ledger_reader::get_tool_usage_stats() {
        Ok(tool_entries) => {
            if !tool_entries.is_empty() {
                println!();
                println!("{:<22} {:<22} {:>8} {:>12}", "Server", "Tool", "Calls", "Last Used");
                println!("{}", "-".repeat(68));
                for e in &tool_entries {
                    let tool = e.tool_name.as_deref().unwrap_or("(none)");
                    println!("{:<22} {:<22} {:>8} {:>12}", e.server_id, tool, e.total_calls, e.last_used_timestamp);
                }
            }
        }
        Err(e) => eprintln!("Error reading tool usage: {}", e),
    }
}

fn run_watch(interval_secs: u64) {
    use std::collections::HashMap;
    use std::time::SystemTime;

    println!("Starting agenttrim watch daemon (poll interval: {interval_secs}s)");
    println!("Watching ~/.agents/skills/ for SKILL.md changes...");

    let skills_dir = dirs::home_dir()
        .expect("HOME must be set")
        .join(".agents")
        .join("skills");

    if !skills_dir.exists() {
        println!("Skills directory does not exist: {}", skills_dir.display());
        println!("Create it or run `agentalign migrate` first.");
        return;
    }

    let mut known_mtimes: HashMap<String, Option<SystemTime>> = HashMap::new();

    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }

                let skill_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };

                let skill_md = path.join("SKILL.md");
                let current_mtime = skill_md.exists().then(|| {
                    std::fs::metadata(&skill_md)
                        .ok()
                        .and_then(|m| m.modified().ok())
                }).flatten();

                let prev_mtime = known_mtimes.get(&skill_name).copied().flatten();

                if current_mtime != prev_mtime || !known_mtimes.contains_key(&skill_name) {
                    if let Err(e) = ledger_reader::log_usage(&skill_name, None, "agenttrim-watch", None) {
                        eprintln!("  ✗ Error logging usage for '{}': {}", skill_name, e);
                    } else {
                        println!("  [{now}] logged usage: {skill_name}");
                    }
                    known_mtimes.insert(skill_name.clone(), current_mtime);
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(interval_secs));
    }
}

fn run_vacuum(dry_run: bool) {
    println!("🧹 vacuum — cleaning orphaned MCP subprocesses");

    if dry_run {
        println!("  → DRY RUN: no processes will be killed");
    }

    // In production, this would come from the canonical config.
    // For now, we use an empty server list to find process orphans
    // by known-agent parent detection only.
    let mcp_servers: HashMap<String, McpServerDefinition> = HashMap::new();

    match crate::prune::subprocess::teardown_orphaned_processes(&mcp_servers, dry_run) {
        Ok(killed) => {
            if killed.is_empty() {
                println!("  ✓ No orphaned MCP processes found.");
            } else {
                for proc in &killed {
                    if dry_run {
                        println!("  [DRY_RUN] would kill PID {} — {}", proc.pid, proc.command);
                    } else {
                        println!(
                            "  [{}] PID {} terminated: {}",
                            proc.signal, proc.pid, proc.command
                        );
                    }
                }
                println!("  → {} process(es) handled.", killed.len());
            }
        }
        Err(e) => {
            eprintln!("  ✗ Error scanning processes: {e}");
        }
    }
}
