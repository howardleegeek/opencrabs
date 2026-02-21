//! Web3 Test Tool
//!
//! Runs smart contract tests via shell-run and returns test report path.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Web3 Test Tool
pub struct Web3TestTool;

/// Default path to shell-run binary
const DEFAULT_SHELL_RUN: &str = "/Users/howardli/.local/bin/shell-run";

#[derive(Debug, Deserialize, Serialize)]
struct Web3TestInput {
    /// Chain to test (default: evm)
    #[serde(default = "default_chain")]
    chain: String,

    /// Optional: path to shell-run binary
    #[serde(skip_serializing_if = "Option::is_none")]
    shell_run_path: Option<String>,

    /// Optional: working directory (overrides context)
    #[serde(skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,

    /// Optional: timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,
}

fn default_chain() -> String {
    "evm".to_string()
}

#[async_trait]
impl Tool for Web3TestTool {
    fn name(&self) -> &str {
        "web3_test"
    }

    fn description(&self) -> &str {
        "Run smart contract tests via shell-run and return path to test report. \
         Truth lives in the JSON report, not stdout."
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
            ToolCapability::Network,
        ]
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let input: Web3TestInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;
        
        if input.chain.is_empty() {
            return Err(ToolError::InvalidInput("chain cannot be empty".to_string()));
        }
        
        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: Web3TestInput = serde_json::from_value(input)?;

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
        
        // Build command
        let mut cmd = Command::new(&shell_run);
        cmd.arg("test")
           .arg("--chain")
           .arg(&input.chain)
           .current_dir(&working_dir);

        // Determine timeout
        let effective_timeout = input.timeout_secs.unwrap_or(300).min(600);

        // Execute command
        let output = match timeout(Duration::from_secs(effective_timeout), cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Ok(ToolResult::error(format!(
                    "Failed to execute shell-run: {}",
                    e
                )));
            }
            Err(_) => {
                return Ok(ToolResult::error(format!(
                    "Command timed out after {} seconds",
                    effective_timeout
                )));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        // Build output - include both stdout/stderr for debugging
        let mut result_text = String::new();
        result_text.push_str(&format!("Exit code: {}\n\n", exit_code));
        
        if !stdout.is_empty() {
            result_text.push_str("STDOUT:\n");
            result_text.push_str(&stdout);
            result_text.push_str("\n");
        }
        
        if !stderr.is_empty() {
            result_text.push_str("STDERR:\n");
            result_text.push_str(&stderr);
        }

        // Determine report path
        let report_path = working_dir
            .join("reports")
            .join(format!("test.{}.forge.json", input.chain));

        let report_exists = report_path.exists();
        
        // Build result with metadata
        let mut result = if output.status.success() {
            ToolResult::success(result_text)
        } else {
            ToolResult {
                success: false,
                output: result_text,
                error: Some(format!("Tests failed with exit code {}", exit_code)),
                metadata: std::collections::HashMap::new(),
            }
        };

        // Add metadata
        result = result
            .with_metadata("exit_code".to_string(), exit_code.to_string())
            .with_metadata("chain".to_string(), input.chain)
            .with_metadata("report_path".to_string(), report_path.display().to_string())
            .with_metadata("report_exists".to_string(), report_exists.to_string());

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_web3_test_tool_schema() {
        let tool = Web3TestTool;
        assert_eq!(tool.name(), "web3_test");
        assert!(tool.requires_approval());
        
        let capabilities = tool.capabilities();
        assert!(capabilities.contains(&ToolCapability::ExecuteShell));
    }

    #[test]
    fn test_validate_empty_chain() {
        let tool = Web3TestTool;
        let input = serde_json::json!({
            "chain": ""
        });
        
        let result = tool.validate_input(&input);
        assert!(result.is_err());
    }

    #[test]
    fn test_default_chain() {
        let input: Web3TestInput = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(input.chain, "evm");
    }
}
