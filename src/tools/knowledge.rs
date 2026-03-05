use async_trait::async_trait;
use tracing::debug;

use super::{Tool, ToolContext, ToolOutput};
use crate::error::Result;
use crate::memory::knowledge::KnowledgeGraph;

/// Knowledge graph tool — exposes graph operations to the LLM.
pub struct KnowledgeGraphTool;

impl KnowledgeGraphTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for KnowledgeGraphTool {
    fn name(&self) -> &str {
        "knowledge_graph"
    }

    fn description(&self) -> &str {
        "Interact with the knowledge graph. Actions: add_node, add_edge, search, neighbors, traverse, update_node, remove_node, remove_edge, stats."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add_node", "add_edge", "search", "neighbors", "traverse",
                             "update_node", "remove_node", "remove_edge", "stats"],
                    "description": "Knowledge graph action"
                },
                "label": { "type": "string", "description": "Node label (for add_node)" },
                "node_type": { "type": "string", "description": "Node type (for add_node)" },
                "content": { "type": "string", "description": "Node content (for add_node, update_node)" },
                "confidence": { "type": "number", "description": "Confidence score 0-1 (for add_node, update_node)" },
                "source_id": { "type": "integer", "description": "Source node ID (for add_edge)" },
                "target_id": { "type": "integer", "description": "Target node ID (for add_edge)" },
                "relation": { "type": "string", "description": "Edge relation type (for add_edge, neighbors, traverse)" },
                "weight": { "type": "number", "description": "Edge weight (for add_edge)" },
                "node_id": { "type": "integer", "description": "Node ID (for neighbors, traverse, update_node, remove_node)" },
                "edge_id": { "type": "integer", "description": "Edge ID (for remove_edge)" },
                "query": { "type": "string", "description": "Search query (for search)" },
                "limit": { "type": "integer", "description": "Max results (for search)" },
                "relations": { "type": "array", "items": { "type": "string" }, "description": "Relation types to traverse" },
                "max_depth": { "type": "integer", "description": "Max traversal depth (for traverse, default 3)" }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or_default();
        let kg = KnowledgeGraph::new(ctx.db.clone(), ctx.db_read.clone());

        debug!(action, "knowledge graph action");

        match action {
            "add_node" => {
                let label = params.get("label").and_then(|v| v.as_str()).unwrap_or_default();
                let node_type = params.get("node_type").and_then(|v| v.as_str()).unwrap_or("");
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let confidence = params.get("confidence").and_then(|v| v.as_f64()).unwrap_or(1.0);

                if label.is_empty() {
                    return Ok(ToolOutput::error("label is required for add_node"));
                }

                let id = kg.add_node(label, node_type, content, confidence).await?;
                Ok(ToolOutput::ok_with_meta(
                    format!("Added node '{label}' (id={id})"),
                    serde_json::json!({ "node_id": id }),
                ))
            }
            "add_edge" => {
                let source_id = params.get("source_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let target_id = params.get("target_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let relation = params.get("relation").and_then(|v| v.as_str()).unwrap_or_default();
                let weight = params.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0);

                if source_id == 0 || target_id == 0 || relation.is_empty() {
                    return Ok(ToolOutput::error("source_id, target_id, and relation are required"));
                }

                let id = kg.add_edge(source_id, target_id, relation, weight).await?;
                Ok(ToolOutput::ok_with_meta(
                    format!("Added edge {source_id} --{relation}--> {target_id} (id={id})"),
                    serde_json::json!({ "edge_id": id }),
                ))
            }
            "search" => {
                let query = params.get("query").and_then(|v| v.as_str()).unwrap_or_default();
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

                if query.is_empty() {
                    return Ok(ToolOutput::error("query is required for search"));
                }

                let nodes = kg.search(query, limit).await?;
                if nodes.is_empty() {
                    Ok(ToolOutput::ok("No matching nodes found."))
                } else {
                    let mut out = String::new();
                    for n in &nodes {
                        out.push_str(&format!(
                            "[{}] {} (type={}, confidence={:.2}): {}\n",
                            n.id, n.label, n.node_type, n.confidence, n.content
                        ));
                    }
                    Ok(ToolOutput::ok(out))
                }
            }
            "neighbors" => {
                let node_id = params.get("node_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let relation = params.get("relation").and_then(|v| v.as_str());

                if node_id == 0 {
                    return Ok(ToolOutput::error("node_id is required"));
                }

                let neighbors = kg.neighbors(node_id, relation).await?;
                if neighbors.is_empty() {
                    Ok(ToolOutput::ok("No neighbors found."))
                } else {
                    let mut out = String::new();
                    for (edge, node) in &neighbors {
                        out.push_str(&format!(
                            "--{}--> [{}] {} ({})\n",
                            edge.relation, node.id, node.label, node.node_type
                        ));
                    }
                    Ok(ToolOutput::ok(out))
                }
            }
            "traverse" => {
                let node_id = params.get("node_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let max_depth = params.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
                let relations: Vec<&str> = params
                    .get("relations")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();

                if node_id == 0 {
                    return Ok(ToolOutput::error("node_id is required"));
                }

                let nodes = kg.traverse(node_id, &relations, max_depth).await?;
                if nodes.is_empty() {
                    Ok(ToolOutput::ok("No reachable nodes found."))
                } else {
                    let mut out = String::new();
                    for n in &nodes {
                        out.push_str(&format!(
                            "[{}] {} (type={}, confidence={:.2})\n",
                            n.id, n.label, n.node_type, n.confidence
                        ));
                    }
                    Ok(ToolOutput::ok(out))
                }
            }
            "update_node" => {
                let node_id = params.get("node_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let content = params.get("content").and_then(|v| v.as_str());
                let confidence = params.get("confidence").and_then(|v| v.as_f64());

                if node_id == 0 {
                    return Ok(ToolOutput::error("node_id is required"));
                }

                kg.update_node(node_id, content, confidence).await?;
                Ok(ToolOutput::ok(format!("Updated node {node_id}")))
            }
            "remove_node" => {
                let node_id = params.get("node_id").and_then(|v| v.as_i64()).unwrap_or(0);
                if node_id == 0 {
                    return Ok(ToolOutput::error("node_id is required"));
                }
                kg.remove_node(node_id).await?;
                Ok(ToolOutput::ok(format!("Removed node {node_id} and its edges")))
            }
            "remove_edge" => {
                let edge_id = params.get("edge_id").and_then(|v| v.as_i64()).unwrap_or(0);
                if edge_id == 0 {
                    return Ok(ToolOutput::error("edge_id is required"));
                }
                kg.remove_edge(edge_id).await?;
                Ok(ToolOutput::ok(format!("Removed edge {edge_id}")))
            }
            "stats" => {
                let (nodes, edges) = kg.stats().await?;
                Ok(ToolOutput::ok(format!("Knowledge graph: {nodes} nodes, {edges} edges")))
            }
            other => Ok(ToolOutput::error(format!("unknown action: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::messaging::MessagingManager;
    use crate::security::SandboxedFs;
    use crate::trash::TrashManager;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        let base = std::env::temp_dir().join(format!("sa-kgtest-{}", std::process::id()));
        let sandbox_dir = base.join("sandbox");
        let trash_dir = base.join("trash");
        std::fs::create_dir_all(&sandbox_dir).unwrap();
        std::fs::create_dir_all(&trash_dir).unwrap();

        let db = db::test_db();
        let db_read = db.clone();

        ToolContext {
            sandbox: SandboxedFs::new(sandbox_dir).unwrap(),
            db,
            db_read,
            http_client: reqwest::Client::new(),
            messaging: Arc::new(MessagingManager::new()),
            trash: Arc::new(TrashManager::new(&trash_dir).unwrap()),
        }
    }

    #[tokio::test]
    async fn add_node_success() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let result = tool.execute(
            serde_json::json!({"action": "add_node", "label": "Rust", "node_type": "language", "content": "Systems language", "confidence": 0.95}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Added node 'Rust'"));
        assert!(result.metadata.is_some());
    }

    #[tokio::test]
    async fn add_node_missing_label() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let result = tool.execute(
            serde_json::json!({"action": "add_node", "node_type": "t"}),
            &ctx,
        ).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("label is required"));
    }

    #[tokio::test]
    async fn add_edge_success() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let kg = KnowledgeGraph::new(ctx.db.clone(), ctx.db_read.clone());
        let a = kg.add_node("A", "t", "", 1.0).await.unwrap();
        let b = kg.add_node("B", "t", "", 1.0).await.unwrap();
        let result = tool.execute(
            serde_json::json!({"action": "add_edge", "source_id": a, "target_id": b, "relation": "knows"}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("knows"));
    }

    #[tokio::test]
    async fn add_edge_missing_fields() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let result = tool.execute(
            serde_json::json!({"action": "add_edge", "source_id": 1}),
            &ctx,
        ).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn search_finds_results() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let kg = KnowledgeGraph::new(ctx.db.clone(), ctx.db_read.clone());
        kg.add_node("Quick brown fox", "animal", "The fox jumps", 1.0).await.unwrap();
        let result = tool.execute(
            serde_json::json!({"action": "search", "query": "brown fox"}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("fox"));
    }

    #[tokio::test]
    async fn search_empty_query() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let result = tool.execute(
            serde_json::json!({"action": "search", "query": ""}),
            &ctx,
        ).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn neighbors_success() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let kg = KnowledgeGraph::new(ctx.db.clone(), ctx.db_read.clone());
        let a = kg.add_node("X", "t", "", 1.0).await.unwrap();
        let b = kg.add_node("Y", "t", "", 1.0).await.unwrap();
        kg.add_edge(a, b, "links", 1.0).await.unwrap();
        let result = tool.execute(
            serde_json::json!({"action": "neighbors", "node_id": a}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Y"));
    }

    #[tokio::test]
    async fn neighbors_missing_node_id() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let result = tool.execute(
            serde_json::json!({"action": "neighbors"}),
            &ctx,
        ).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn stats_action() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let result = tool.execute(
            serde_json::json!({"action": "stats"}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("0 nodes"));
    }

    #[tokio::test]
    async fn unknown_action() {
        let ctx = test_ctx();
        let tool = KnowledgeGraphTool::new();
        let result = tool.execute(
            serde_json::json!({"action": "foobar"}),
            &ctx,
        ).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("unknown action"));
    }

    #[tokio::test]
    async fn tool_metadata() {
        let tool = KnowledgeGraphTool::new();
        assert_eq!(tool.name(), "knowledge_graph");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["required"].as_array().unwrap().contains(&serde_json::json!("action")));
    }
}
