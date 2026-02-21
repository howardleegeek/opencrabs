//! Web3 Report Read Tool
//!
//! Reads test/deploy/audit reports and returns structured summary.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;

/// Web3 Report Read Tool
pub struct Web3ReportReadTool;

#[derive(Debug, Deserialize, Serialize)]
struct Web3ReportReadInput {
    /// Path to the report file (JSON)
    report_path: String,

    /// Optional: report type hint (test, deploy, audit)
    #[serde(skip_serializing_if = "Option::is_none")]
    report_type: Option<String>,
}

/// Structured test result summary
#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct TestReportSummary {
    ok: bool,
    total_tests: u32,
    passed: u32,
    failed: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<TestFailure>,
    raw_log_path: String,
}

#[derive(Debug, Serialize)]
struct TestFailure {
    test: String,
    reason: String,
}

/// Structured deploy result summary
#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct DeployReportSummary {
    ok: bool,
    contract_address: String,
    network: String,
    block_number: Option<u64>,
    transaction_hash: Option<String>,
    raw_log_path: String,
}

/// Structured audit result summary
#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct AuditReportSummary {
    ok: bool,
    issues_found: u32,
    critical: u32,
    high: u32,
    medium: u32,
    low: u32,
    raw_log_path: String,
}

#[async_trait]
impl Tool for Web3ReportReadTool {
    fn name(&self) -> &str {
        "web3_report_read"
    }

    fn description(&self) -> &str {
        "Read test/deploy/audit reports from JSON files and return structured summary. \
         Use this after web3_test or web3_deploy to check results. This tool enables \
         debugger behavior by parsing test failures and deployment status."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "report_path": {
                    "type": "string",
                    "description": "Path to the JSON report file"
                },
                "report_type": {
                    "type": "string",
                    "description": "Optional: report type hint (test, deploy, audit)",
                    "enum": ["test", "deploy", "audit"]
                }
            },
            "required": ["report_path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadFiles]
    }

    fn requires_approval(&self) -> bool {
        false // Read-only operation
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let input: Web3ReportReadInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;
        
        if input.report_path.is_empty() {
            return Err(ToolError::InvalidInput("report_path cannot be empty".to_string()));
        }
        
        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: Web3ReportReadInput = serde_json::from_value(input)?;

        // Resolve report path - could be absolute or relative to working dir
        let report_path = if PathBuf::from(&input.report_path).is_absolute() {
            PathBuf::from(&input.report_path)
        } else {
            context.working_directory.join(&input.report_path)
        };

        // Check if file exists
        if !report_path.exists() {
            return Ok(ToolResult::error(format!(
                "Report file not found: {}",
                report_path.display()
            )));
        }

        // Read and parse JSON
        let content = fs::read_to_string(&report_path).await
            .map_err(|e| ToolError::Execution(format!("Failed to read report: {}", e)))?;

        let json: Value = serde_json::from_str(&content)
            .map_err(|e| ToolError::Execution(format!("Failed to parse JSON: {}", e)))?;

        // Determine report type and parse accordingly
        let report_type = input.report_type
            .or_else(|| detect_report_type(&report_path))
            .unwrap_or_else(|| "unknown".to_string());

        let (ok, summary) = match report_type.as_str() {
            "test" => parse_test_report(&json),
            "deploy" => parse_deploy_report(&json),
            "audit" => parse_audit_report(&json),
            _ => parse_generic_report(&json),
        };

        // Serialize summary to JSON
        let summary_json = serde_json::to_string_pretty(&summary)
            .map_err(|e| ToolError::Execution(format!("Failed to serialize summary: {}", e)))?;

        // Build result
        let result = ToolResult::success(summary_json)
            .with_metadata("report_path".to_string(), report_path.display().to_string())
            .with_metadata("report_type".to_string(), report_type)
            .with_metadata("ok".to_string(), ok.to_string())
            .with_metadata("raw_log_path".to_string(), report_path.display().to_string());

        Ok(result)
    }
}

/// Detect report type from file path
fn detect_report_type(path: &PathBuf) -> Option<String> {
    let path_str = path.to_string_lossy().to_lowercase();
    if path_str.contains("test") {
        Some("test".to_string())
    } else if path_str.contains("deploy") {
        Some("deploy".to_string())
    } else if path_str.contains("audit") {
        Some("audit".to_string())
    } else {
        None
    }
}

/// Parse test report - handles Foundry forge format
fn parse_test_report(json: &Value) -> (bool, HashMap<String, Value>) {
    let mut summary = HashMap::new();
    let mut failures = Vec::new();
    let mut passed = 0u32;
    let mut failed = 0u32;

    // Try to extract test results from various formats
    // Foundry forge format
    if let Some(test_results) = json.get("results") {
        if let Some(results_obj) = test_results.as_object() {
            for (test_name, result) in results_obj {
                if let Some(result_obj) = result.as_object() {
                    let success = result_obj.get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    
                    if success {
                        passed += 1;
                    } else {
                        failed += 1;
                        let reason = result_obj.get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Unknown failure")
                            .to_string();
                        failures.push(TestFailure {
                            test: test_name.clone(),
                            reason,
                        });
                    }
                }
            }
        }
    }

    // Alternative format: top-level success/Stats
    // Alternative format: top-level success/Stats
    let _ok = if let Some(success) = json.get("success").and_then(|v| v.as_bool()) {
        success
    } else if let Some(stats) = json.get("Stats") {
        let total = stats.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let passed_val = stats.get("passed").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        passed = passed_val;
        failed = total.saturating_sub(passed_val);
        failed > 0
    } else {
        // Default: check if there's a failure array
        json.get("failures").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(true)
    };

    // Override ok based on actual counts
    let ok = passed > 0 && failed == 0;

    summary.insert("ok".to_string(), serde_json::json!(ok));
    summary.insert("total_tests".to_string(), serde_json::json!(passed + failed));
    summary.insert("passed".to_string(), serde_json::json!(passed));
    summary.insert("failed".to_string(), serde_json::json!(failed));
    summary.insert("failures".to_string(), serde_json::json!(failures));

    (ok, summary)
}

/// Parse deploy report
fn parse_deploy_report(json: &Value) -> (bool, HashMap<String, Value>) {
    let mut summary = HashMap::new();

    let ok = json.get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let contract_address = json.get("address")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let network = json.get("network")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let block_number = json.get("blockNumber")
        .and_then(|v| v.as_u64());

    let transaction_hash = json.get("transactionHash")
        .or_else(|| json.get("tx"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    summary.insert("ok".to_string(), serde_json::json!(ok));
    summary.insert("contract_address".to_string(), serde_json::json!(contract_address));
    summary.insert("network".to_string(), serde_json::json!(network));
    summary.insert("block_number".to_string(), serde_json::json!(block_number));
    summary.insert("transaction_hash".to_string(), serde_json::json!(transaction_hash));

    (ok, summary)
}

/// Parse audit report
fn parse_audit_report(json: &Value) -> (bool, HashMap<String, Value>) {
    let mut summary = HashMap::new();

    // Slither or other audit tool format
    let issues = json.get("results")
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u32)
        .unwrap_or(0);

    let critical = json.get("critical")
        .or_else(|| json.get("Critical"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let high = json.get("high")
        .or_else(|| json.get("High"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let medium = json.get("medium")
        .or_else(|| json.get("Medium"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let low = json.get("low")
        .or_else(|| json.get("Low"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let ok = critical == 0 && high == 0;

    summary.insert("ok".to_string(), serde_json::json!(ok));
    summary.insert("issues_found".to_string(), serde_json::json!(issues));
    summary.insert("critical".to_string(), serde_json::json!(critical));
    summary.insert("high".to_string(), serde_json::json!(high));
    summary.insert("medium".to_string(), serde_json::json!(medium));
    summary.insert("low".to_string(), serde_json::json!(low));

    (ok, summary)
}

/// Parse generic report
fn parse_generic_report(json: &Value) -> (bool, HashMap<String, Value>) {
    let mut summary = HashMap::new();

    // Try common success fields
    let ok = json.get("ok")
        .or_else(|| json.get("success"))
        .or_else(|| json.get("passed"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    summary.insert("ok".to_string(), serde_json::json!(ok));
    summary.insert("raw".to_string(), json.clone());

    (ok, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web3_report_read_tool_schema() {
        let tool = Web3ReportReadTool;
        assert_eq!(tool.name(), "web3_report_read");
        assert!(!tool.requires_approval());
        
        let capabilities = tool.capabilities();
        assert!(capabilities.contains(&ToolCapability::ReadFiles));
    }

    #[test]
    fn test_validate_empty_path() {
        let tool = Web3ReportReadTool;
        let input = serde_json::json!({
            "report_path": ""
        });
        
        let result = tool.validate_input(&input);
        assert!(result.is_err());
    }

    #[test]
    fn test_detect_report_type() {
        let path = PathBuf::from("/project/reports/test.evm.forge.json");
        let detected = detect_report_type(&path);
        assert_eq!(detected, Some("test".to_string()));
    }

    #[test]
    fn test_parse_generic_report() {
        let json = serde_json::json!({
            "ok": true,
            "data": "some data"
        });
        
        let (ok, summary) = parse_generic_report(&json);
        assert!(ok);
        assert!(summary.contains_key("ok"));
    }
}
