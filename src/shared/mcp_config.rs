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

    if let Ok(map) = serde_json::from_str::<HashMap<String, McpServerDefinition>>(&content) {
        return Ok(map);
    }

    #[derive(serde::Deserialize)]
    struct Wrapper {
        mcp: HashMap<String, McpServerDefinition>,
    }
    if let Ok(wrapper) = serde_json::from_str::<Wrapper>(&content) {
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
