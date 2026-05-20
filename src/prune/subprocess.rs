use anyhow::Result;
use std::collections::HashMap;
use std::time::Duration;
use sysinfo::{Pid, ProcessesToUpdate, Signal, System};

use agentalign_shared::models::McpServerDefinition;

use super::super::analyze::process_scanner::{find_mcp_processes, McpProcess};

/// Record of a process kill operation.
#[derive(Debug, Clone)]
pub struct KilledProcess {
    pub pid: u32,
    pub command: String,
    /// Signal that successfully terminated the process ("SIGTERM" or "SIGKILL")
    pub signal: String,
    pub success: bool,
}

/// Refresh a single process's info in the system state.
fn refresh_single(system: &mut System, pid: u32) {
    system.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), false);
}

/// Attempt graceful then forced termination of a single process.
///
/// If `graceful` is true, sends SIGTERM and waits 3 seconds for the process
/// to exit. If still alive after the wait, sends SIGKILL.
///
/// Returns `Ok(true)` if the process was successfully terminated.
pub fn teardown_process(pid: u32, graceful: bool) -> Result<bool> {
    let mut system = System::new();
    refresh_single(&mut system, pid);

    // Safety: never kill PID 1 or self
    if pid <= 1 || pid == std::process::id() {
        return Ok(false);
    }

    let process = match system.process(Pid::from_u32(pid)) {
        Some(p) => p,
        None => return Ok(false), // Process already exited
    };

    if graceful {
        // Send SIGTERM for graceful shutdown
        if process.kill_with(Signal::Term).unwrap_or(false) {
            // Wait up to 3 seconds for graceful exit
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
                refresh_single(&mut system, pid);
                if system.process(Pid::from_u32(pid)).is_none() {
                    return Ok(true); // Process exited gracefully
                }
            }

            // Process still alive after grace period — force kill
            refresh_single(&mut system, pid);
            if let Some(p) = system.process(Pid::from_u32(pid)) {
                return Ok(p.kill_with(Signal::Kill).unwrap_or(false));
            }
            return Ok(true); // Process died between checks
        }
    }

    // Direct SIGKILL (or fallback if graceful failed)
    refresh_single(&mut system, pid);
    if let Some(p) = system.process(Pid::from_u32(pid)) {
        Ok(p.kill_with(Signal::Kill).unwrap_or(false))
    } else {
        Ok(true) // Already dead
    }
}

/// Find all orphaned MCP subprocesses and terminate them.
///
/// Returns a report of what was killed, which signal was used, and whether
/// each operation succeeded.
///
/// Does NOT perform any signal sending if `dry_run` is true.
pub fn teardown_orphaned_processes(
    mcp_servers: &HashMap<String, McpServerDefinition>,
    dry_run: bool,
) -> Result<Vec<KilledProcess>> {
    let processes = find_mcp_processes(mcp_servers)?;
    let orphans: Vec<McpProcess> = processes.into_iter().filter(|p| p.is_orphan).collect();

    if dry_run {
        return Ok(orphans
            .into_iter()
            .map(|p| KilledProcess {
                pid: p.pid,
                command: p.command,
                signal: "DRY_RUN".to_string(),
                success: true,
            })
            .collect());
    }

    let mut results = Vec::new();

    for proc in orphans {
        // Try graceful first — track which signal succeeds
        let signal = if try_graceful_kill(proc.pid)? {
            "SIGTERM"
        } else {
            // Graceful failed, try force kill
            if try_force_kill(proc.pid)? {
                "SIGKILL"
            } else {
                results.push(KilledProcess {
                    pid: proc.pid,
                    command: proc.command,
                    signal: "NONE".to_string(),
                    success: false,
                });
                continue;
            }
        };

        results.push(KilledProcess {
            pid: proc.pid,
            command: proc.command,
            signal: signal.to_string(),
            success: true,
        });
    }

    Ok(results)
}

/// Send SIGTERM and wait for process to exit. Returns true if process died.
fn try_graceful_kill(pid: u32) -> Result<bool> {
    let mut system = System::new();
    refresh_single(&mut system, pid);

    let process = match system.process(Pid::from_u32(pid)) {
        Some(p) => p,
        None => return Ok(true),
    };

    if process.kill_with(Signal::Term).unwrap_or(false) {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
            refresh_single(&mut system, pid);
            if system.process(Pid::from_u32(pid)).is_none() {
                return Ok(true);
            }
        }
        return Ok(false); // Still alive after grace period
    }

    Ok(false) // SIGTERM failed or not sent
}

/// Send SIGKILL. Returns true if process died or was already dead.
fn try_force_kill(pid: u32) -> Result<bool> {
    let mut system = System::new();
    refresh_single(&mut system, pid);

    match system.process(Pid::from_u32(pid)) {
        Some(p) => Ok(p.kill_with(Signal::Kill).unwrap_or(false)),
        None => Ok(true), // Already dead
    }
}
