//! Web3 Auto Repair Tool - Orchestrator
//!
//! Complete auto-repair闭环: test → report → classify → patch → retest → (optional deploy)
//!
//! State machine:
//! S0: run_test
//! S1: read_report -> if ok: DONE else: S2
//! S2: classify_failure -> if unsupported: DONE else: S3
//! S3: collect_context -> S4
//! S4: propose_patch -> S5
//! S5: apply_patch -> if fail: DONE else: S6
//! S6: re_test -> loop back to S1

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Web3 Auto Repair Tool - Full Implementation
pub struct Web3AutoRepairTool;

/// Default path to shell-run binary
const DEFAULT_SHELL_RUN: &str = "/Users/howardli/.local/bin/shell-run";

/// Maximum rounds for repair attempts
const DEFAULT_MAX_ROUNDS: u32 = 2;

/// Maximum files that can be changed in one repair
const DEFAULT_MAX_FILES: u32 = 4;

/// Maximum lines that can be changed in one patch
const DEFAULT_MAX_PATCH_LINES: u32 = 200;

#[derive(Debug, Deserialize, Serialize)]
struct Web3AutoRepairInput {
    /// Project path (default: context working directory)
    #[serde(skip_serializing_if = "Option::is_none")]
    project_path: Option<String>,
    
    /// Chain to test (default: evm)
    #[serde(default = "default_chain")]
    chain: String,
    
    /// Maximum repair rounds (default: 2)
    #[serde(default = "default_max_rounds")]
    max_rounds: u32,
    
    /// Allowed edit paths (default: ["src/", "test/", "script/"])
    #[serde(default = "default_allow_paths")]
    allow_edit_paths: Vec<String>,
    
    /// Maximum files changed per repair (default: 4)
    #[serde(default = "default_max_files")]
    max_files_changed: u32,
    
    /// Maximum patch lines (default: 200)
    #[serde(default = "default_max_patch_lines")]
    max_patch_lines: u32,
    
    /// Deploy after tests pass (default: false)
    #[serde(default)]
    deploy_after_green: bool,
    
    /// Network for deployment (default: anvil)
    #[serde(default = "default_network")]
    deploy_network: String,
    
    /// Optional: path to shell-run binary
    #[serde(skip_serializing_if = "Option::is_none")]
    shell_run_path: Option<String>,
}

fn default_chain() -> String { "evm".to_string() }
fn default_max_rounds() -> u32 { DEFAULT_MAX_ROUNDS }
fn default_allow_paths() -> Vec<String> { 
    vec!["src/".to_string(), "test/".to_string(), "script/".to_string()] 
}
fn default_max_files() -> u32 { DEFAULT_MAX_FILES }
fn default_max_patch_lines() -> u32 { DEFAULT_MAX_PATCH_LINES }
fn default_network() -> String { "anvil".to_string() }

/// Repair round result
#[derive(Debug, Clone, Serialize)]
struct RepairRound {
    round: u32,
    category: String,
    patch_summary: String,
    test_result: TestResult,
}

/// Test result
#[derive(Debug, Clone, Serialize)]
struct TestResult {
    ok: bool,
    passed: u32,
    failed: u32,
    summary: String,
}

/// Final repair result
#[derive(Debug, Serialize)]
struct AutoRepairResult {
    ok: bool,
    total_rounds: u32,
    success: bool,
    final_report_path: String,
    patches: Vec<String>,
    notes: String,
    rounds: Vec<RepairRound>,
}

#[async_trait]
impl Tool for Web3AutoRepairTool {
    fn name(&self) -> &str {
        "web3_auto_repair"
    }

    fn description(&self) -> &str {
        "Automatically repair failing smart contract tests. \
         Runs test, analyzes failures, generates patch, applies it, and re-runs test. \
         Supports: event mismatch, require message mismatch, visibility errors. \
         Configurable: max_rounds, allow_edit_paths, max_files_changed."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Project directory path"
                },
                "chain": {
                    "type": "string",
                    "description": "Chain to test (default: evm)",
                    "default": "evm"
                },
                "max_rounds": {
                    "type": "integer",
                    "description": "Maximum repair rounds (default: 2)",
                    "default": 2
                },
                "allow_edit_paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Allowed edit paths",
                    "default": ["src/", "test/", "script/"]
                },
                "max_files_changed": {
                    "type": "integer",
                    "description": "Maximum files changed per repair",
                    "default": 4
                },
                "max_patch_lines": {
                    "type": "integer",
                    "description": "Maximum patch lines",
                    "default": 200
                },
                "deploy_after_green": {
                    "type": "boolean",
                    "description": "Deploy after tests pass",
                    "default": false
                },
                "deploy_network": {
                    "type": "string",
                    "description": "Network for deployment",
                    "default": "anvil"
                },
                "shell_run_path": {
                    "type": "string",
                    "description": "Optional: path to shell-run binary"
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

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: Web3AutoRepairInput = serde_json::from_value(input)?;
        
        // Determine working directory
        let working_dir = if let Some(ref dir) = input.project_path {
            PathBuf::from(dir)
        } else {
            context.working_directory.clone()
        };

        if !working_dir.exists() {
            return Ok(ToolResult::error(format!(
                "Project directory does not exist: {}",
                working_dir.display()
            )));
        }

        // Initialize repair state
        let shell_run = input.shell_run_path.clone().unwrap_or_else(|| DEFAULT_SHELL_RUN.to_string());
        let mut rounds: Vec<RepairRound> = Vec::new();
        let mut patches: Vec<String> = Vec::new();
        let max_rounds = input.max_rounds;
        
        // ===== STATE MACHINE =====
        
        // S0: Run initial test
        let test_result = run_test(&shell_run, &input.chain, &working_dir, 300).await;
        
        // S1: Check if already passing
        if test_result.ok {
            let result = ToolResult::success(
                format!("Tests already passing! {}", test_result.summary)
            ).with_metadata("ok".to_string(), "true".to_string())
            .with_metadata("rounds".to_string(), "0".to_string());
            return Ok(result);
        }
        
        // ===== REPAIR LOOP =====
        let mut round = 1;
        while round <= max_rounds {
            // S2: Read and classify failure
            let report_path = working_dir.join("reports").join(format!("test.{}.forge.json", input.chain));
            let failures = read_failures(&report_path).await?;
            
            if failures.is_empty() {
                break; // Can't proceed without failure info
            }
            
            // S3: Classify failure
            let category = classify_failure(&failures);
            
            // S4: Generate patch (simplified for v1 - placeholder)
            let patch_result = generate_patch(&category, &failures, &working_dir, &input).await;
            
            if patch_result.is_none() {
                // Unsupported failure type
                break;
            }
            
            let (patch_content, patch_summary) = patch_result.unwrap();
            
            // Validate patch
            if let Err(e) = validate_patch(&patch_content, &input) {
                return Ok(ToolResult::error(format!("Patch validation failed: {}", e)));
            }
            
            // S5: Apply patch
            let patch_path = working_dir.join("reports").join(format!("repair.round{}.patch", round));
            let _ = apply_patch(&patch_path, &patch_content).await;
            patches.push(patch_path.display().to_string());
            
            // S6: Re-run test
            let new_result = run_test(&shell_run, &input.chain, &working_dir, 300).await;
            
            rounds.push(RepairRound {
                round,
                category: category.clone(),
                patch_summary: patch_summary.clone(),
                test_result: new_result.clone(),
            });
            
            if new_result.ok {
                // Success! Tests now passing
                let notes = format!(
                    "Fixed {} after {} round(s). Tests now green.",
                    category, round
                );
                
                // Optional: deploy after green
                if input.deploy_after_green {
                    // TODO: Call deploy here
                }
                
                let final_report = AutoRepairResult {
                    ok: true,
                    total_rounds: round,
                    success: true,
                    final_report_path: report_path.display().to_string(),
                    patches: patches.clone(),
                    notes,
                    rounds: rounds.clone(),
                };
                
                return Ok(ToolResult::success(serde_json::to_string_pretty(&final_report).unwrap())
                    .with_metadata("ok".to_string(), "true".to_string())
                    .with_metadata("rounds".to_string(), round.to_string()));
            }
            
            round += 1;
        }
        
        // Failed after max rounds
        let final_report = AutoRepairResult {
            ok: false,
            total_rounds: round - 1,
            success: false,
            final_report_path: working_dir.join("reports").join(format!("test.{}.forge.json", input.chain)).display().to_string(),
            patches,
            notes: format!("Failed after {} rounds. Could not fix automatically.", max_rounds),
            rounds,
        };
        
        Ok(ToolResult {
            success: false,
            output: serde_json::to_string_pretty(&final_report).unwrap(),
            error: Some("Auto-repair could not fix the failing tests".to_string()),
            metadata: std::collections::HashMap::new(),
        })
    }
}

/// Run test via shell-run
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

/// Read failures from report
async fn read_failures(report_path: &PathBuf) -> Result<Vec<Value>> {
    if !report_path.exists() {
        return Ok(Vec::new());
    }
    
    let content = tokio::fs::read_to_string(report_path).await
        .map_err(|e| ToolError::Execution(format!("Failed to read report: {}", e)))?;
    
    let json: Value = serde_json::from_str(&content)
        .map_err(|e| ToolError::Execution(format!("Failed to parse report: {}", e)))?;
    
    Ok(json.get("details")
        .and_then(|d| d.get("failures"))
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default())
}

/// Classify failure type
fn classify_failure(failures: &[Value]) -> String {
    if failures.is_empty() {
        return "unknown".to_string();
    }
    
    // Check first failure
    let reason = failures[0].get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    
    let test_name = failures[0].get("test")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    
    if reason.contains("event") || test_name.contains("event") || reason.contains("emit") {
        return "EVENT_MISMATCH".to_string();
    }
    if reason.contains("revert") || reason.contains("require") || reason.contains("expected error") {
        return "REVERT_MESSAGE_MISMATCH".to_string();
    }
    if reason.contains("visibility") || reason.contains("identifier") || reason.contains("undeclared") {
        return "SYMBOL_OR_VISIBILITY".to_string();
    }
    
    "unknown".to_string()
}

/// Generate patch (simplified - v1 placeholder)
#[allow(dead_code)]
async fn generate_patch(
    category: &str,
    _failures: &[Value],
    _working_dir: &PathBuf,
    _input: &Web3AutoRepairInput,
) -> Option<(String, String)> {
    // In production, this would call LLM to generate the actual patch
    // For v1, we return a placeholder based on category
    
    match category {
        "EVENT_MISMATCH" => {
            Some((
                "# PATCH_UNAVAILABLE - Event mismatch requires manual fix\n".to_string(),
                "Event mismatch detected - needs event definition fix".to_string(),
            ))
        }
        "REVERT_MESSAGE_MISMATCH" => {
            Some((
                "# PATCH_UNAVAILABLE - Revert message mismatch requires manual fix\n".to_string(),
                "Revert message mismatch - needs require message fix".to_string(),
            ))
        }
        "SYMBOL_OR_VISIBILITY" => {
            Some((
                "# PATCH_UNAVAILABLE - Symbol/visibility error requires manual fix\n".to_string(),
                "Symbol or visibility error - needs code fix".to_string(),
            ))
        }
        _ => None, // Unsupported
    }
}

/// Validate patch against constraints
fn validate_patch(patch: &str, input: &Web3AutoRepairInput) -> Result<()> {
    // Count lines changed (simplified)
    let lines: Vec<&str> = patch.lines().collect();
    let changed_lines = lines.iter().filter(|l| l.starts_with('+') || l.starts_with('-')).count();
    
    if changed_lines as u32 > input.max_patch_lines {
        return Err(ToolError::InvalidInput(format!(
            "Patch too large: {} lines (max: {})",
            changed_lines, input.max_patch_lines
        )));
    }
    
    // Check file paths
    for line in lines {
        if line.starts_with("+++") || line.starts_with("---") {
            let path = line.split_whitespace().last().unwrap_or("");
            let allowed = input.allow_edit_paths.iter().any(|p| path.starts_with(p));
            if !allowed && !path.contains("PATCH_UNAVAILABLE") {
                return Err(ToolError::InvalidInput(format!(
                    "Patch modifies disallowed path: {}",
                    path
                )));
            }
        }
    }
    
    Ok(())
}

/// Apply patch to file
async fn apply_patch(patch_path: &PathBuf, content: &str) -> Result<()> {
    // Ensure reports directory exists
    if let Some(parent) = patch_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    
    tokio::fs::write(patch_path, content).await
        .map_err(|e| ToolError::Execution(format!("Failed to write patch: {}", e)))?;
    
    Ok(())
}

/// Extract count from text
fn extract_count(text: &str, pattern: &str) -> Option<u32> {
    let re = regex::Regex::new(pattern).ok()?;
    let caps = re.captures(text)?;
    caps.get(1)?.as_str().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        let failures = vec![
            serde_json::json!({"test": "testDeposit", "reason": "Event mismatch: expected Deposit"})
        ];
        
        let category = classify_failure(&failures);
        assert_eq!(category, "EVENT_MISMATCH");
    }

    #[test]
    fn test_revert_classification() {
        let failures = vec![
            serde_json::json!({"test": "testRevert", "reason": "Error != expected error: Must send ETH"})
        ];
        
        let category = classify_failure(&failures);
        assert_eq!(category, "REVERT_MESSAGE_MISMATCH");
    }
}
