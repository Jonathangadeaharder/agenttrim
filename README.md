# agenttrim

Telemetry-Driven Pruning & Vacuum Engine for AI agent environments. Scans `~/.agents/` for unused skills and MCP servers by cross-referencing SQLite + JSON usage ledgers, kills orphaned MCP subprocesses, and removes stale artifacts — all behind safety gates and pre-purge validation.

Version: 0.1.0 · Rust edition 2021 · MIT licensed

## CLI Commands

```text
agenttrim analyze   Scan and report unused resources
agenttrim prune     Remove unused resources (with safety gates)
agenttrim vacuum    Deep clean: kill orphaned MCP subprocesses
agenttrim status    Show usage stats from SQLite ledger
agenttrim watch     Daemon: watch skill/MCP filesystem for access, log usage
agenttrim config    Configure thresholds and allowlists (stub)
```

### `analyze`

```bash
agenttrim analyze [--agents-root <PATH>] [--mcp-config <PATH>] \
                  [--projects-root <PATH>] [--threshold-days <N>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--agents-root` | `~/.agents/` | Path to agents directory |
| `--mcp-config` | `~/.agents/mcp_config.json` | Path to canonical MCP config |
| `--projects-root` | _(none)_ | Optional projects root for static reference scanning |
| `--threshold-days` | `90` | Inactivity threshold in days |

### `prune`

```bash
agenttrim prune [--agents-root <PATH>] [--mcp-config <PATH>] \
                [--force] [--dry-run]
```

| Flag | Description |
|------|-------------|
| `--agents-root` | Path to agents directory (default `~/.agents/`) |
| `--mcp-config` | Path to canonical MCP config (default `~/.agents/mcp_config.json`) |
| `--force` | Non-interactive mode (still passes safety gates) |
| `--dry-run` | Show what would be pruned without deleting |

### `vacuum`

```bash
agenttrim vacuum [--dry-run]
```

Kills orphaned MCP subprocesses (parent process dead). Graceful SIGTERM → 3s wait → SIGKILL.

### `status`

```bash
agenttrim status
```

Reads `~/.agents/usage.db` and prints per-server and per-tool usage tables.

### `watch`

```bash
agenttrim watch [--interval <SECONDS>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--interval` | `60` | Poll interval in seconds |

Daemon that watches `~/.agents/skills/*/SKILL.md` for mtime changes and logs usage to the SQLite ledger.

## Architecture

### Directory Layout

```text
src/
├── main.rs                  # CLI entrypoint (clap subcommands)
├── lib.rs                   # Module re-exports
├── time_provider.rs         # TimeProvider trait (injectable clock for testing)
├── analyze/
│   ├── mod.rs               # FullAnalysisReport, run_full_analysis()
│   ├── skills.rs            # SkillAnalyzer — skill usage vs ledger + safety matrix
│   ├── mcp.rs               # McpAnalyzer — MCP server health + usage + duplicates
│   ├── process_scanner.rs   # find_mcp_processes() — match running procs against config
│   ├── safety_matrix.rs     # Hardcoded never-prune allowlist (exact + glob patterns)
│   ├── static_scanner.rs    # Grep-style scan for skill name references in project files
│   ├── validation_hook.rs   # PrePurgeValidation — safety gates before deletion
│   └── ledger_reader.rs     # SQLite + JSON usage ledger I/O
├── prune/
│   ├── mod.rs               # PruneReport, prune_skills_unified(), prune_mcp_unified()
│   ├── skills.rs            # prune_skills() — remove skill dirs/symlinks
│   ├── mcp.rs               # prune_mcp_servers() — remove entries from config JSON
│   ├── backup.rs            # create_backup(), restore_backup(), list_backups()
│   └── subprocess.rs        # teardown_orphaned_processes(), teardown_process()
└── shared/
    ├── mod.rs
    ├── models.rs            # UnusedReport, McpServerDefinition, ReportKind, UsageEntry, etc.
    ├── mcp_config.rs        # load_mcp_config() — parses flat or { mcp: ... } JSON
    ├── error.rs             # AdapterError, TrimError
    └── traits.rs            # ConfigurationAdapter, McpFormatStrategy
```

### ~/.agents/ Layout

```text
~/.agents/
├── usage.db                  # SQLite usage ledger (auto-created)
├── .skill-usage.json         # JSON skill usage ledger (supplementary)
├── mcp_config.json           # Canonical MCP server definitions
├── skills/                   # Installed skill directories
│   └── <skill-name>/
│       └── SKILL.md
└── backups/                  # Timestamped pre-prune snapshots
    └── YYYY-MM-DD-pre-prune-*/
```

### Safety Matrix

The following items are protected and can never be pruned:

| ID | Reason |
|----|--------|
| `agent-browser` | Critical browser automation infrastructure |
| `find-skills` | Skill discovery system |
| `supabase` | Persistent cloud service config |
| `test-review` | Mandatory CI quality gate |
| `filesystem` | Core filesystem access tool |
| `postgres-*` | Database executor (glob pattern) |
| `sequential-*` | Sequential thinking tools (glob pattern) |

### Prune Pipeline

1. **Analyze** — SkillAnalyzer + McpAnalyzer cross-reference telemetry against threshold
2. **Validate** — PrePurgeValidation checks: safety matrix, path still exists, mtime older than 7 days, backup writable
3. **Confirm** — Interactive prompt (skipped with `--force` or `--dry-run`)
4. **Backup** — Timestamped snapshot to `~/.agents/backups/`
5. **Delete** — Remove skill dirs/symlinks, or remove MCP entries from config JSON

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `clap` | 4 | CLI argument parsing (derive) |
| `serde` | 1 | Serialization (derive) |
| `serde_json` | 1 | JSON parsing/emission |
| `toml_edit` | 0.22 | TOML config manipulation (reserved, not yet active in code paths) |
| `thiserror` | 2 | Error type derives |
| `anyhow` | 1 | Error propagation |
| `dirs` | 5 | Home directory resolution |
| `rusqlite` | 0.31 | SQLite usage ledger (bundled) |
| `sysinfo` | 0.33 | Process inspection |
| `libc` | 0.2 | UID checks (Unix) |
| `walkdir` | 2 | Recursive directory traversal |
| `tar` | 0.4 | Archive support |
| `flate2` | 1 | Compression |

Dev: `tempfile` 3

## Build & Run

```bash
cargo build
cargo test
cargo clippy -- -D warnings

# Usage examples
cargo run -- analyze
cargo run -- analyze --threshold-days 30
cargo run -- prune --dry-run
cargo run -- vacuum --dry-run
cargo run -- status
cargo run -- watch --interval 30
```
