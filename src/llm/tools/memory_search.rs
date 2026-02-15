//! Memory Search Tool
//!
//! Searches past conversation compaction logs using QMD.
//! If QMD is not installed, returns a helpful hint instead of failing.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

/// Memory search tool backed by QMD.
pub struct MemorySearchTool;

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search past conversation memory logs for relevant context. \
         Use this when you need to recall decisions, files, errors, or context \
         from previous sessions. Returns matching excerpts from daily memory logs."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language search query for past memories"
                },
                "n": {
                    "type": "integer",
                    "description": "Number of results to return (default: 5)",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadFiles]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if query.is_empty() {
            return Ok(ToolResult::error("query parameter is required".to_string()));
        }

        let n = input
            .get("n")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        // Check if QMD is available
        if !crate::memory::is_qmd_available() {
            return Ok(ToolResult::success(
                "QMD is not installed. Memory search requires QMD (https://github.com/qmd-project/qmd). \
                 Install it to enable searching past conversation logs. \
                 Daily memory logs are still saved to ~/.opencrabs/memory/ as markdown files \
                 that you can read directly with the read_file tool."
                    .to_string(),
            ));
        }

        // Ensure collection exists
        if let Err(e) = crate::memory::ensure_collection() {
            tracing::warn!("Failed to ensure QMD collection: {}", e);
        }

        // Search
        match crate::memory::search(&query, n) {
            Ok(results) => {
                if results.trim().is_empty() {
                    Ok(ToolResult::success(
                        "No matching memories found.".to_string(),
                    ))
                } else {
                    Ok(ToolResult::success(results))
                }
            }
            Err(e) => Ok(ToolResult::error(format!("Memory search failed: {}", e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_metadata() {
        let tool = MemorySearchTool;
        assert_eq!(tool.name(), "memory_search");
        assert!(!tool.requires_approval());
    }

    #[tokio::test]
    async fn test_empty_query() {
        let tool = MemorySearchTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let result = tool
            .execute(serde_json::json!({"query": ""}), &ctx)
            .await
            .unwrap();
        assert!(!result.success);
    }
}
