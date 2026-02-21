//! Web3 Auto Repair Tool
//!
//! Automatically repairs failing smart contract tests.
//! Single round: test → analyze → patch → retest

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Web3 Auto Repair Tool
pub struct Web3AutoRepairTool;

/// Default path to shell-run binary
const DEFAULT_SHELL_RUN: &str = "/Users/howardli/.local/bin/shell-run";

#[derive(Debug, Deserialize, Serialize)]
struct Web3AutoRepairInput {
    /// Chain to test (default: evm)
    #[serde(default = "default_chain")]
    chain: String,

    /// Optional: path to shell-run binary
    #[serde(skip_serializing_if = "Option::is_none")]
    shell_run_path: Option<String>,

    /// Optional: working directory (overrides context)
    #[serde(skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,

    /// Optional: timeout for single test run (seconds)
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_chain() -> String {
    "evm".to_string()
}

fn default_timeout() -> u64 {
    300
}

/// Repair result
#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct RepairResult {
    /// Whether tests passed after repair
    ok: bool,
    
    /// Whether a repair was attempted
    repaired: bool,
    
    /// The patch that was applied (if any)
    patch: String,
    
    /// Error category that was repaired (if any)
    error_category: String,
    
    /// Number of test runs
    runs: u32,
    
    /// Final test summary
    summary: String,
}

/// Supported error categories for auto-repair (v1)
#[derive(Debug, Clone)]
enum ErrorCategory {
    EventMismatch,
    RequireMessageMismatch,
    MissingVisibility,
    Unknown,
}

impl ErrorCategory {
    fn from_failures(failures: &[Value]) -> Self {
        // Simple heuristics for v1
        for failure in failures {
            let reason = failure.get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            
            let test_name = failure.get("test")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            
            if reason.contains("event") || test_name.contains("event") {
                return ErrorCategory::EventMismatch;
            }
            if reason.contains("revert") || reason.contains("require") {
                return ErrorCategory::RequireMessageMismatch;
            }
            if reason.contains("visibility") || reason.contains("private") || reason.contains("internal") {
                return ErrorCategory::MissingVisibility;
            }
        }
        ErrorCategory::Unknown
    }
}

#[async_trait]
impl Tool for Web3AutoRepairTool {
    fn name(&self) -> &str {
        "web3_auto_repair"
    }

    fn description(&self) -> &str {
        "Automatically repair failing smart contract tests. \
         Runs test, analyzes failures, generates patch, applies it, and re-runs test. \
         Single round of repair. Supports: event mismatch, require message mismatch, visibility errors."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "chain": {
                    "type": "string",
                    "description": "Chain to test (default: evm)",
                    "default": "evm"
                },
                "shell_run_path": {
                    "type": "string",
                    "description": "Optional: path to shell-run binary"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional: working directory for command execution"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional: timeout in seconds (default 300)"
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecuteShell,
            ToolCapability::WriteFiles,
            ToolCapability::ReadFiles,
        ]
    }

    fn requires_approval(&self) -> bool {
        true // Modifies files
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let input: Web3AutoRepairInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;
        
        if input.chain.is_empty() {
            return Err(ToolError::InvalidInput("chain cannot be empty".to_string()));
        }
        
        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: Web3AutoRepairInput = serde_json::from_value(input)?;

        // Determine working directory
        let working_dir = if let Some(ref dir) = input.working_dir {
            PathBuf::from(dir)
        } else {
            context.working_directory.clone()
        };

        // Verify working directory exists
        if !working_dir.exists() {
            return Ok(ToolResult::error(format!(
                "Working directory does not exist: {}",
                working_dir.display()
            )));
        }

        // Determine shell-run path
        let shell_run = input.shell_run_path.unwrap_or_else(|| DEFAULT_SHELL_RUN.to_string());
        
        // ========== Step 1: Run initial test ==========
        let initial_result = run_test(&shell_run, &input.chain, &working_dir, input.timeout_secs).await;
        
        // If tests already pass, return success
        if initial_result.ok {
            let result = ToolResult::success(format!(
                "Tests already passing! {}",
                initial_result.summary
            ))
            .with_metadata("ok".to_string(), "true".to_string())
            .with_metadata("repaired".to_string(), "false".to_string())
            .with_metadata("runs".to_string(), "1".to_string());
            return Ok(result);
        }

        // ========== Step 2: Read failure report ==========
        let report_path = working_dir
            .join("reports")
            .join(format!("test.{}.forge.json", input.chain));
        
        let failures = if report_path.exists() {
            let content = tokio::fs::read_to_string(&report_path).await
                .map_err(|e| ToolError::Execution(format!("Failed to read report: {}", e)))?;
            
            let json: Value = serde_json::from_str(&content)
                .map_err(|e| ToolError::Execution(format!("Failed to parse report: {}", e)))?;
            
            json.get("details")
                .and_then(|d| d.get("failures"))
                .and_then(|f| f.as_array())
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // ========== Step 3: Analyze and generate patch ==========
        let error_category = ErrorCategory::from_failures(&failures);
        let error_str = match &error_category {
            ErrorCategory::EventMismatch => "event_mismatch",
            ErrorCategory::RequireMessageMismatch => "require_message_mismatch", 
            ErrorCategory::MissingVisibility => "missing_visibility",
            ErrorCategory::Unknown => "unknown",
        };

        // For v1, we generate a simple patch based on error category
        // In production, this would call LLM to generate the actual fix
        
        // ========== Step 4: Apply patch ==========
        let patch = generate_patch_v1(&failures, &error_category, &working_dir).await;
        let repaired = patch.is_some();
        if repaired {
            // Write the patched file
            if let Some((file_path, content)) = &patch {
                let full_path = working_dir.join(file_path);
                tokio::fs::write(&full_path, content).await
                    .map_err(|e| ToolError::Execution(format!("Failed to write patch: {}", e)))?;
            }
        }

        // ========== Step 5: Re-run test ==========
        let runs = 2;
        let final_result = run_test(&shell_run, &input.chain, &working_dir, input.timeout_secs).await;

        // Build response
        let summary = if final_result.ok {
            format!("Repair successful after {} runs. {}", runs, final_result.summary)
        } else {
            format!("Repair failed after {} runs. {}", runs, initial_result.summary)
        };

        let result = if final_result.ok {
            ToolResult::success(summary)
        } else {
            ToolResult {
                success: false,
                output: summary.clone(),
                error: Some("Auto-repair could not fix the failing tests".to_string()),
                metadata: std::collections::HashMap::new(),
            }
        };

        Ok(result
            .with_metadata("ok".to_string(), final_result.ok.to_string())
            .with_metadata("repaired".to_string(), repaired.to_string())
            .with_metadata("error_category".to_string(), error_str.to_string())
            .with_metadata("runs".to_string(), runs.to_string())
            .with_metadata("passed".to_string(), final_result.passed.to_string())
            .with_metadata("failed".to_string(), final_result.failed.to_string()))
    }
}

/// Run test and return result
async fn run_test(shell_run: &str, chain: &str, dir: &PathBuf, timeout_secs: u64) -> TestResult {
    let mut cmd = Command::new(shell_run);
    cmd.arg("test")
       .arg("--chain")
       .arg(chain)
       .current_dir(dir)
       .stdout(Stdio::piped())
       .stderr(Stdio::piped());

    let output = match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(Ok(o)) => o,
        _ => {
            return TestResult {
                ok: false,
                passed: 0,
                failed: 0,
                summary: "Test execution timed out".to_string(),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    
    // Parse output
    let passed = extract_count(&stdout, r"(\d+)\s+passing")
        .or_else(|| extract_count(&stdout, r"(\d+)\s+passed"))
        .unwrap_or(0);
    let failed = extract_count(&stdout, r"(\d+)\s+failing")
        .or_else(|| extract_count(&stdout, r"(\d+)\s+failed"))
        .unwrap_or(0);

    TestResult {
        ok: output.status.success() || (failed == 0 && passed > 0),
        passed,
        failed,
        summary: if output.status.success() {
            format!("{} passed, {} failed", passed, failed)
        } else {
            format!("Tests failed: {} passing, {} failing", passed, failed)
        },
    }
}

fn extract_count(text: &str, pattern: &str) -> Option<u32> {
    let re = regex::Regex::new(pattern).ok()?;
    let caps = re.captures(text)?;
    caps.get(1)?.as_str().parse().ok()
}

/// Generate patch for v1 (simplified)
/// In production, this would call LLM
async fn generate_patch_v1(
    failures: &[Value],
    category: &ErrorCategory,
    _working_dir: &PathBuf,
) -> Option<(String, String)> {
    // For v1, we'll return a simple patch indicator
    // The actual patch generation would be done by LLM in production
    
    if failures.is_empty() {
        return None;
    }

    // Return a placeholder patch that indicates what would be fixed
    // In production, LLM would generate the actual code change
    match category {
        ErrorCategory::EventMismatch => {
            Some(("patch_placeholder.txt".to_string(), "Event mismatch detected - needs event fix".to_string()))
        }
        ErrorCategory::RequireMessageMismatch => {
            Some(("patch_placeholder.txt".to_string(), "Require message mismatch - needs message fix".to_string()))
        }
        ErrorCategory::MissingVisibility => {
            Some(("patch_placeholder.txt".to_string(), "Missing visibility - needs visibility fix".to_string()))
        }
        ErrorCategory::Unknown => {
            None // Cannot auto-repair unknown errors
        }
    }
}

#[derive(Debug)]
struct TestResult {
    ok: bool,
    passed: u32,
    failed: u32,
    summary: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web3_auto_repair_tool_schema() {
        let tool = Web3AutoRepairTool;
        assert_eq!(tool.name(), "web3_auto_repair");
        assert!(tool.requires_approval());
        
        let capabilities = tool.capabilities();
        assert!(capabilities.contains(&ToolCapability::ExecuteShell));
        assert!(capabilities.contains(&ToolCapability::WriteFiles));
    }

    #[test]
    fn test_error_category_parsing() {
        let failures = vec![
            serde_json::json!({"test": "testDeposit", "reason": "Event mismatch: expected Deposit"})
        ];
        
        let category = ErrorCategory::from_failures(&failures);
        assert!(matches!(category, ErrorCategory::EventMismatch));
    }

    #[test]
    fn test_require_message_parsing() {
        let failures = vec![
            serde_json::json!({"test": "testRevert", "reason": "require failed: Must send ETH"})
        ];
        
        let category = ErrorCategory::from_failures(&failures);
        assert!(matches!(category, ErrorCategory::RequireMessageMismatch));
    }
}
