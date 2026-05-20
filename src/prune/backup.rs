use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

/// Create a timestamped snapshot backup of specified paths.
///
/// Backups are stored at `~/.agents/backups/YYYY-MM-DD-<name>/`
/// and contain verbatim copies of all files from the provided paths.
///
/// Returns the path to the backup directory.
pub fn create_backup(name: &str, paths: &[&Path]) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    let backup_root = home.join(".agents").join("backups");

    let date_stamp = chrono_stamp();
    let backup_dir = backup_root.join(format!("{date_stamp}-{name}"));

    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup directory: {}", backup_dir.display()))?;

    for src in paths {
        let src_canonical = fs::canonicalize(src)
            .with_context(|| format!("Cannot canonicalize source path: {}", src.display()))?;

        if src_canonical.is_dir() {
            let dest_dir = backup_dir.join(
                src_canonical
                    .file_name()
                    .unwrap_or_default(),
            );
            copy_dir_recursive(&src_canonical, &dest_dir)?;
        } else {
            let dest = backup_dir.join(
                src_canonical
                    .file_name()
                    .unwrap_or_default(),
            );
            fs::copy(&src_canonical, &dest)
                .with_context(|| format!("Failed to copy {} to {}", src.display(), dest.display()))?;
        }
    }

    Ok(backup_dir)
}

/// List all available backups, ordered most-recent-first.
pub fn list_backups() -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    let backup_root = home.join(".agents").join("backups");

    if !backup_root.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<PathBuf> = fs::read_dir(&backup_root)
        .with_context(|| format!("Cannot read backup directory: {}", backup_root.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();

    // Sort by modified time, most recent first
    entries.sort_by(|a, b| {
        let a_time = fs::metadata(a)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let b_time = fs::metadata(b)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        b_time.cmp(&a_time)
    });

    Ok(entries)
}

/// Restore files from a backup directory to their original locations.
///
/// Uses the backup directory name to infer the correct subdirectory target.
/// - If name contains "skills" → restore to `~/.agents/skills/`
/// - Otherwise → restore to `~/.agents/`
///
/// This prevents skill backups from being dropped into the wrong parent directory.
pub fn restore_backup(backup_path: &Path) -> Result<()> {
    if !backup_path.is_dir() {
        anyhow::bail!("Backup path is not a directory: {}", backup_path.display());
    }

    let home = dirs::home_dir().context("Cannot determine home directory")?;
    let agents_root = home.join(".agents");

    let is_skill_backup = backup_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.contains("skills"))
        .unwrap_or(false);

    let target_root = if is_skill_backup {
        agents_root.join("skills")
    } else {
        agents_root
    };

    for entry in WalkDir::new(backup_path).min_depth(1).max_depth(1) {
        let entry = entry?;
        let src = entry.path();
        let file_name = src.file_name().unwrap_or_default();
        let dest = target_root.join(file_name);

        if src.is_dir() {
            copy_dir_recursive(src, &dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(src, &dest)?;
        }
    }

    Ok(())
}

/// Recursively copy a directory from src to dest.
fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;

    for entry in WalkDir::new(src).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(src)?;
        let target = dest.join(relative);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }

    Ok(())
}

/// Generate a compact ISO-8601 date stamp (YYYY-MM-DD).
fn chrono_stamp() -> String {
    // Use system time formatting instead of pulling in the chrono crate
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple conversion to YYYY-MM-DD
    // Days since epoch
    let days = secs / 86400;
    let remaining = secs % 86400;
    let _hours = remaining / 3600;
    let _minutes = (remaining % 3600) / 60;
    let _seconds = remaining % 60;

    // Civil date from days since epoch (Rata Die algorithm)
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrono_stamp_format() {
        let stamp = chrono_stamp();
        assert_eq!(stamp.len(), 10, "Expected YYYY-MM-DD format");
        assert_eq!(&stamp[4..5], "-", "Expected dash at position 4");
        assert_eq!(&stamp[7..8], "-", "Expected dash at position 7");
        // Should all be digits otherwise
        let parts: Vec<&str> = stamp.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 4); // year
        assert_eq!(parts[1].len(), 2); // month
        assert_eq!(parts[2].len(), 2); // day
    }
}
