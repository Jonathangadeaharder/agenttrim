use anyhow::Result;
use std::collections::HashMap;
use sysinfo::{ProcessesToUpdate, System};

use agentalign_shared::models::McpServerDefinition;

/// A running process that may be an MCP server subprocess.
#[derive(Debug, Clone)]
pub struct McpProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub command: String,
    /// Parent process no longer running
    pub is_orphan: bool,
    /// Which MCP server definition this matches, if any
    pub matched_server: Option<String>,
}

/// Browsers/editors known to spawn MCP subprocesses.
const KNOWN_AGENTS: &[&str] = &[
    "claude",
    "cursor",
    "code",
    "windsurf",
    "github copilot",
    "continue",
];

/// Enumerate all running processes and match against MCP server definitions and known agent parents.
///
/// Returns every process that either:
/// - Matches a known MCP server command from `mcp_servers`
/// - Has a parent PID belonging to a known agent that is no longer running (orphan)
pub fn find_mcp_processes(
    mcp_servers: &HashMap<String, McpServerDefinition>,
) -> Result<Vec<McpProcess>> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, false);

    let processes = system.processes();
    let mut results: Vec<McpProcess> = Vec::new();

    // Build set of known MCP binary names from server definitions
    let known_mcp_binaries: Vec<String> = mcp_servers
        .values()
        .filter_map(|def| {
            def.command
                .as_ref()
                .and_then(|cmd| cmd.first().cloned())
        })
        .collect();

    for (pid, process) in processes {
        let pid_u32 = pid.as_u32();
        let parent_pid = process.parent().map(|p| p.as_u32()).unwrap_or(0);

        // Safety: skip init/system processes
        if pid_u32 <= 1 || parent_pid <= 1 {
            continue;
        }

        // Safety: never kill processes outside our UID
        #[cfg(unix)]
        {
            let current_uid = unsafe { libc::getuid() };
            if let Some(uid) = process.user_id() {
                let uid_str = format!("{uid:?}");
                if !uid_str.contains(&current_uid.to_string()) {
                    continue;
                }
            }
        }
        #[cfg(windows)]
        {
            // On Windows, sysinfo captures user-space boundaries natively.
            // The process table is already filtered to the user's session.
            // No additional UID check needed.
        }

        // Convert OsString cmd to string for matching
        let cmd_line: Vec<String> = process
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        let cmd_full = cmd_line.join(" ");
        let cmd_first = cmd_line.first().cloned().unwrap_or_default().to_lowercase();

        // Check if this process matches a known MCP binary
        let matched_server = known_mcp_binaries.iter().find_map(|binary| {
            if cmd_full.contains(binary.as_str())
                || cmd_first.contains(&binary.to_lowercase())
            {
                // Find the server name that has this binary
                mcp_servers.iter().find_map(|(name, def)| {
                    def.command
                        .as_ref()
                        .and_then(|c| c.first())
                        .filter(|b| *b == binary)
                        .map(|_| name.clone())
                })
            } else {
                None
            }
        });

        // Determine if parent is still alive
        let parent_alive = processes.iter().any(|(p, _)| p.as_u32() == parent_pid);

        // Check if parent is a known agent
        let parent_is_agent = if parent_alive {
            processes
                .iter()
                .find(|(p, _)| p.as_u32() == parent_pid)
                .map(|(_, p)| {
                    let pname = p.name().to_string_lossy().to_lowercase();
                    KNOWN_AGENTS.iter().any(|a| pname.contains(a))
                })
                .unwrap_or(false)
        } else {
            false
        };

        // Process is orphaned if parent process is no longer running
        let is_orphan = !parent_alive && parent_pid != 0;

        // Secondary orphan detection: any process whose command path resembles an MCP server
        let resembles_mcp_subprocess = cmd_full.contains("mcp")
            || cmd_full.contains("evals")
            || known_mcp_binaries.iter().any(|b| cmd_full.contains(b.as_str()));

        // Include process if:
        // 1. It directly matches a known MCP server binary, OR
        // 2. It looks like an MCP subprocess AND its parent is dead
        // 3. Its parent was a known agent but is now dead
        let is_relevant = matched_server.is_some()
            || (resembles_mcp_subprocess && is_orphan)
            || (parent_is_agent && is_orphan);

        if is_relevant {
            results.push(McpProcess {
                pid: pid_u32,
                parent_pid,
                command: cmd_full,
                is_orphan,
                matched_server,
            });
        }
    }

    Ok(results)
}
