//! Web3 Deploy Tool
//!
//! Deploys smart contracts via shell-run and returns deployment info.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Web3 Deploy Tool
pub struct Web3DeployTool;

/// Default path to shell-run binary
const DEFAULT_SHELL_RUN: &str = "shell-run";

#[derive(Debug, Deserialize, Serialize)]
struct Web3DeployInput {
    /// Network to deploy to (e.g., anvil, sepolia, mainnet)
    #[serde(default = "default_network")]
    network: String,

    /// Chain to use (e.g., evm)
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

    /// Optional: constructor arguments
    #[serde(skip_serializing_if = "Option::is_none")]
    constructor_args: Option<Vec<String>>,
}

fn default_network() -> String {
    "anvil".to_string()
}

fn default_chain() -> String {
    "evm".to_string()
}

#[async_trait]
impl Tool for Web3DeployTool {
    fn name(&self) -> &str {
        "web3_deploy"
    }

    fn description(&self) -> &str {
        "Deploy smart contracts via shell-run to specified network. \
         Returns contract address, network, and report path. \
         Use after tests pass (verify with web3_report_read)."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "network": {
                    "type": "string",
                    "description": "Network to deploy to (e.g., anvil, sepolia, mainnet)",
                    "default": "anvil"
                },
                "chain": {
                    "type": "string",
                    "description": "Chain to use (e.g., evm)",
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
                },
                "constructor_args": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Optional: constructor arguments"
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecuteShell,
            ToolCapability::Network,
            ToolCapability::SystemModification,
        ]
    }

    fn requires_approval(&self) -> bool {
        true // Deployment modifies state
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let input: Web3DeployInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;
        
        if input.network.is_empty() {
            return Err(ToolError::InvalidInput("network cannot be empty".to_string()));
        }
        
        if input.chain.is_empty() {
            return Err(ToolError::InvalidInput("chain cannot be empty".to_string()));
        }
        
        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: Web3DeployInput = serde_json::from_value(input)?;

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
        
        // Build command: shell-run deploy --network <network> --chain <chain>
        let mut cmd = Command::new(&shell_run);
        cmd.arg("deploy")
           .arg("--network")
           .arg(&input.network)
           .arg("--chain")
           .arg(&input.chain)
           .current_dir(&working_dir);

        // Add constructor args if provided
        if let Some(ref args) = input.constructor_args {
            for arg in args {
                cmd.arg("--arg").arg(arg);
            }
        }

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

        // Build output
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
            .join(format!("deploy.{}.json", input.network));

        let report_exists = report_path.exists();

        // Try to extract contract address from output
        let contract_address = extract_address_from_output(&stdout, &stderr);

        // Also try to get from report (only if not found in output)
        let contract_address = if contract_address.is_none() && report_exists {
            match extract_address_from_report(&report_path).await {
                Ok(addr) => addr,
                Err(_) => None,
            }
        } else {
            contract_address
        };

        // Build result
        let mut result = if output.status.success() || contract_address.is_some() {
            ToolResult::success(result_text)
        } else {
            ToolResult {
                success: false,
                output: result_text,
                error: Some(format!("Deployment failed with exit code {}", exit_code)),
                metadata: std::collections::HashMap::new(),
            }
        };

        // Add metadata
        result = result
            .with_metadata("exit_code".to_string(), exit_code.to_string())
            .with_metadata("network".to_string(), input.network.clone())
            .with_metadata("chain".to_string(), input.chain.clone())
            .with_metadata("report_path".to_string(), report_path.display().to_string())
            .with_metadata("report_exists".to_string(), report_exists.to_string());

        if let Some(addr) = contract_address {
            result = result.with_metadata("contract_address".to_string(), addr);
        }

        Ok(result)
    }
}

/// Try to extract contract address from stdout/stderr
fn extract_address_from_output(stdout: &str, stderr: &str) -> Option<String> {
    // Look for common patterns:
    // - "Deployed to: 0x..."
    // - "Contract deployed at: 0x..."
    // - "0x[0-9a-fA-F]{40}" (raw address)
    
    let combined = format!("{}\n{}", stdout, stderr);
    
    // Try known patterns
    for line in combined.lines() {
        let line = line.trim();
        
        // Pattern: "Deployed to: 0x..."
        if line.contains("Deployed to:") || line.contains("Deployed at:") {
            if let Some(addr) = extract_0x_address(line) {
                return Some(addr);
            }
        }
        
        // Pattern: "Contract Address: 0x..." or "address: 0x..."
        if line.to_lowercase().contains("contract address") || 
           line.to_lowercase().contains("address:") {
            if let Some(addr) = extract_0x_address(line) {
                return Some(addr);
            }
        }
    }

    // Fallback: try to find any 0x40 hex string that looks like an address
    for word in combined.split_whitespace() {
        if word.starts_with("0x") && word.len() == 42 {
            // Valid Ethereum address format
            return Some(word.to_string());
        }
    }

    None
}

/// Extract 0x address from a line
fn extract_0x_address(line: &str) -> Option<String> {
    for word in line.split(|c: char| !c.is_alphanumeric()) {
        if word.starts_with("0x") && word.len() == 42 {
            return Some(word.to_string());
        }
    }
    None
}

/// Try to extract address from deployment report file
async fn extract_address_from_report(report_path: &PathBuf) -> Result<Option<String>> {
    if !report_path.exists() {
        return Ok(None);
    }

    let content = tokio::fs::read_to_string(report_path).await
        .map_err(|e| ToolError::Execution(format!("Failed to read report: {}", e)))?;

    let json: Value = serde_json::from_str(&content)
        .map_err(|e| ToolError::Execution(format!("Failed to parse JSON: {}", e)))?;

    Ok(json.get("address")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_web3_deploy_tool_schema() {
        let tool = Web3DeployTool;
        assert_eq!(tool.name(), "web3_deploy");
        assert!(tool.requires_approval());
        
        let capabilities = tool.capabilities();
        assert!(capabilities.contains(&ToolCapability::ExecuteShell));
        assert!(capabilities.contains(&ToolCapability::Network));
    }

    #[test]
    fn test_validate_empty_network() {
        let tool = Web3DeployTool;
        let input = serde_json::json!({
            "network": "",
            "chain": "evm"
        });
        
        let result = tool.validate_input(&input);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_empty_chain() {
        let tool = Web3DeployTool;
        let input = serde_json::json!({
            "network": "anvil",
            "chain": ""
        });
        
        let result = tool.validate_input(&input);
        assert!(result.is_err());
    }

    #[test]
    fn test_default_values() {
        let input: Web3DeployInput = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(input.network, "anvil");
        assert_eq!(input.chain, "evm");
    }

    #[test]
    fn test_extract_address_from_output() {
        let stdout = "Deploying contracts with chain ID: 31337\nDeployed to: 0x1234567890123456789012345678901234567890";
        let addr = extract_address_from_output(stdout, "");
        assert_eq!(addr, Some("0x1234567890123456789012345678901234567890".to_string()));
    }

    #[test]
    fn test_extract_address_case_insensitive() {
        let stdout = "Contract Address: 0xDEADBEEF1234567890123456789012345678901";
        let addr = extract_address_from_output(stdout, "");
        assert_eq!(addr, Some("0xDEADBEEF1234567890123456789012345678901".to_string()));
    }
}
