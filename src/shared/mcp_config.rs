use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use crate::shared::models::McpServerDefinition;

pub fn load_mcp_config(path: &Path) -> Result<HashMap<String, McpServerDefinition>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read MCP config at {:?}", path))?;

    // Parse to Value first to surface JSON syntax errors with line/column info
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse MCP config JSON at {:?}", path))?;

    if let Ok(map) = serde_json::from_value::<HashMap<String, McpServerDefinition>>(parsed.clone()) {
        return Ok(map);
    }

    #[derive(serde::Deserialize)]
    struct Wrapper {
        mcp: HashMap<String, McpServerDefinition>,
    }
    if let Ok(wrapper) = serde_json::from_value::<Wrapper>(parsed) {
        return Ok(wrapper.mcp);
    }

    anyhow::bail!("Cannot parse MCP config: unknown JSON structure at {:?}", path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_mcp_config_not_found() {
        let result = load_mcp_config(Path::new("/nonexistent/config.json")).unwrap();
        assert!(result.is_empty());
    }
}
