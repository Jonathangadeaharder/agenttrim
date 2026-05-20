use anyhow::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Fast recursive grep-style scan for references.
///
/// Searches for `pattern` in all files under `root_dirs` whose extension
/// matches one of `extensions` (e.g., `["rs", "md", "toml"]`).
/// Returns a vector of `(file_path, match_count)` pairs.
pub fn scan_for_references(
    pattern: &str,
    root_dirs: &[&Path],
    extensions: &[&str],
) -> Result<Vec<(PathBuf, usize)>> {
    let mut results = Vec::new();

    for root in root_dirs {
        if !root.exists() || !root.is_dir() {
            continue;
        }

        for entry in WalkDir::new(root).min_depth(1).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();

            // Check extension filter
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());

            let ext_match = match ext {
                Some(ref e) => extensions.is_empty() || extensions.iter().any(|x| *x == e.as_str()),
                None => extensions.is_empty(),
            };

            if !ext_match {
                continue;
            }

            // Skip binary files and hidden files
            if is_binary_or_hidden(path) {
                continue;
            }

            // Count matching lines
            let count = match count_matches(path, pattern) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if count > 0 {
                results.push((path.to_path_buf(), count));
            }
        }
    }

    Ok(results)
}

/// Count lines in a file that contain the given pattern.
fn count_matches(path: &Path, pattern: &str) -> std::io::Result<usize> {
    let content = std::fs::read_to_string(path)?;
    Ok(content.lines().filter(|line| line.contains(pattern)).count())
}

/// Heuristic check: skip files that are likely binary or hidden.
fn is_binary_or_hidden(path: &Path) -> bool {
    // Skip hidden files (starting with '.')
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.starts_with('.') {
            return true;
        }
    }

    // Skip common binary extensions
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let binary_extensions = [
            "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg",
            "woff", "woff2", "ttf", "eot",
            "zip", "gz", "bz2", "xz", "tar",
            "pdf", "doc", "docx", "xls", "xlsx",
            "mp3", "mp4", "avi", "mov", "wav",
            "o", "so", "dylib", "dll", "exe",
            "pyc", "pyo",
            "ttf", "otf",
        ];
        if binary_extensions.contains(&ext) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_scan_for_references() {
        let dir = std::env::temp_dir().join("agenttrim_test_static_scan");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Create test files
        fs::write(dir.join("file_a.rs"), "use agent_browser;\nfn main() {}\n").unwrap();
        fs::write(
            dir.join("file_b.md"),
            "# agent_browser usage\ndocs about agent-browser\n",
        )
        .unwrap();
        fs::write(dir.join("file_c.txt"), "no matches here\n").unwrap();

        let roots = &[dir.as_path()];
        let results =
            scan_for_references("agent_browser", roots, &["rs", "md"]).unwrap();

        assert_eq!(results.len(), 2, "Should find matches in .rs and .md files");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scan_empty_extensions() {
        let dir = std::env::temp_dir().join("agenttrim_test_empty_ext");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("file.txt"), "agent_browser\n").unwrap();
        fs::write(dir.join("file.rs"), "agent_browser\n").unwrap();

        let roots = &[dir.as_path()];
        // Empty extensions = match all
        let results = scan_for_references("agent_browser", roots, &[] as &[&str]).unwrap();
        assert_eq!(results.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_binary_skipped() {
        let dir = std::env::temp_dir().join("agenttrim_test_binary");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Write a .png file (should be skipped even if it has the pattern)
        let png_path = dir.join("icon.png");
        fs::write(&png_path, "agent_browser png content\n").unwrap();

        let roots = &[dir.as_path()];
        let results = scan_for_references("agent_browser", roots, &["png"]).unwrap();
        assert_eq!(results.len(), 0, "Binary files should be skipped");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nonexistent_root() {
        let dir = Path::new("/nonexistent/path/that/does/not/exist");
        let results = scan_for_references("pattern", &[dir], &["rs"]).unwrap();
        assert!(results.is_empty());
    }
}
