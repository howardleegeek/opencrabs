//! Memory Module
//!
//! Provides long-term memory search via QMD (a local document search CLI).
//! If QMD is not installed, gracefully degrades — the memory_search tool
//! returns a hint instead of failing.

use std::path::PathBuf;
use std::process::Command;

/// Name of the QMD collection used for OpenCrabs daily memory logs.
const COLLECTION_NAME: &str = "opencrabs-memory";

/// Check if QMD is installed and available on PATH.
pub fn is_qmd_available() -> bool {
    Command::new("qmd")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ensure the QMD collection exists, pointing at `~/.opencrabs/memory/`.
///
/// Idempotent — safe to call multiple times.
pub fn ensure_collection() -> Result<(), String> {
    let memory_dir = memory_dir();
    std::fs::create_dir_all(&memory_dir)
        .map_err(|e| format!("Failed to create memory dir: {}", e))?;

    let output = Command::new("qmd")
        .args(["collection", "add", &memory_dir.to_string_lossy(), "--name", COLLECTION_NAME])
        .output()
        .map_err(|e| format!("Failed to run qmd collection add: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "already exists" is fine — idempotent
        if !stderr.contains("already exists") {
            return Err(format!("qmd collection add failed: {}", stderr));
        }
    }
    Ok(())
}

/// Search memory logs using QMD.
///
/// Returns the raw JSON output from `qmd query`.
pub fn search(query: &str, n: usize) -> Result<String, String> {
    let output = Command::new("qmd")
        .args([
            "query",
            query,
            "--json",
            "-n",
            &n.to_string(),
            "-c",
            COLLECTION_NAME,
        ])
        .output()
        .map_err(|e| format!("Failed to run qmd query: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("qmd query failed: {}", stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Trigger a background re-index of the memory collection.
///
/// Non-blocking: spawns the process and does not wait for it.
pub fn reindex_background() {
    if !is_qmd_available() {
        return;
    }
    std::thread::spawn(|| {
        let _ = Command::new("qmd")
            .args(["update", "-c", COLLECTION_NAME])
            .output();
    });
}

/// Path to the memory directory: `~/.opencrabs/memory/`
fn memory_dir() -> PathBuf {
    crate::config::opencrabs_home().join("memory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_dir() {
        let dir = memory_dir();
        assert!(dir.to_string_lossy().contains("memory"));
    }
}
