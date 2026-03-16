use std::sync::Arc;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeNode {
    pub id: i64,
    pub label: String,
    pub node_type: String,
    pub content: String,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEdge {
    pub id: i64,
    pub source_id: i64,
    pub target_id: i64,
    pub relation: String,
    pub weight: f64,
    pub metadata: serde_json::Value,
    pub created_at: String,
}

pub struct KnowledgeGraph {
    db: Arc<Mutex<Connection>>,
    db_read: Arc<Mutex<Connection>>,
}

impl KnowledgeGraph {
    /// Create a KnowledgeGraph. Use `db` for writes, `db_read` for reads.
    /// When only one connection is available, pass it for both.
    pub fn new(db: Arc<Mutex<Connection>>, db_read: Arc<Mutex<Connection>>) -> Self {
        Self { db, db_read }
    }

    pub async fn add_node(
        &self,
        label: &str,
        node_type: &str,
        content: &str,
        confidence: f64,
    ) -> Result<i64> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO knowledge_nodes (label, node_type, content, confidence) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![label, node_type, content, confidence],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub async fn add_edge(
        &self,
        source_id: i64,
        target_id: i64,
        relation: &str,
        weight: f64,
    ) -> Result<i64> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT OR IGNORE INTO knowledge_edges (source_id, target_id, relation, weight) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![source_id, target_id, relation, weight],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<KnowledgeNode>> {
        let db = self.db_read.lock().await;
        let mut stmt = db.prepare(
            "SELECT n.id, n.label, n.node_type, n.content, n.confidence, n.created_at, n.updated_at
             FROM knowledge_nodes_fts fts
             JOIN knowledge_nodes n ON n.id = fts.rowid
             WHERE knowledge_nodes_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let nodes = stmt
            .query_map(rusqlite::params![query, limit as i64], |row| {
                Ok(KnowledgeNode {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    node_type: row.get(2)?,
                    content: row.get(3)?,
                    confidence: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(nodes)
    }

    pub async fn get_node(&self, id: i64) -> Result<KnowledgeNode> {
        let db = self.db_read.lock().await;
        let node = db.query_row(
            "SELECT id, label, node_type, content, confidence, created_at, updated_at
             FROM knowledge_nodes WHERE id = ?1",
            [id],
            |row| {
                Ok(KnowledgeNode {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    node_type: row.get(2)?,
                    content: row.get(3)?,
                    confidence: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )?;
        Ok(node)
    }

    pub async fn neighbors(
        &self,
        node_id: i64,
        relation_filter: Option<&str>,
    ) -> Result<Vec<(KnowledgeEdge, KnowledgeNode)>> {
        let db = self.db_read.lock().await;

        let query = if relation_filter.is_some() {
            "SELECT e.id, e.source_id, e.target_id, e.relation, e.weight, e.metadata, e.created_at,
                    n.id, n.label, n.node_type, n.content, n.confidence, n.created_at, n.updated_at
             FROM knowledge_edges e
             JOIN knowledge_nodes n ON n.id = CASE WHEN e.source_id = ?1 THEN e.target_id ELSE e.source_id END
             WHERE (e.source_id = ?1 OR e.target_id = ?1) AND e.relation = ?2"
        } else {
            "SELECT e.id, e.source_id, e.target_id, e.relation, e.weight, e.metadata, e.created_at,
                    n.id, n.label, n.node_type, n.content, n.confidence, n.created_at, n.updated_at
             FROM knowledge_edges e
             JOIN knowledge_nodes n ON n.id = CASE WHEN e.source_id = ?1 THEN e.target_id ELSE e.source_id END
             WHERE (e.source_id = ?1 OR e.target_id = ?1)"
        };

        let mut stmt = db.prepare(query)?;

        let rows = if let Some(rel) = relation_filter {
            stmt.query_map(rusqlite::params![node_id, rel], map_edge_node)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(rusqlite::params![node_id], map_edge_node)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };

        Ok(rows)
    }

    pub async fn traverse(
        &self,
        node_id: i64,
        relations: &[&str],
        max_depth: usize,
    ) -> Result<Vec<KnowledgeNode>> {
        let db = self.db_read.lock().await;

        let relation_clause = if relations.is_empty() {
            String::new()
        } else {
            let placeholders: Vec<String> = relations.iter().map(|r| format!("'{}'", r.replace('\'', "''"))).collect();
            format!("AND e.relation IN ({})", placeholders.join(", "))
        };

        let sql = format!(
            "WITH RECURSIVE reachable(nid, depth) AS (
                SELECT ?1, 0
                UNION
                SELECT CASE WHEN e.source_id = r.nid THEN e.target_id ELSE e.source_id END, r.depth + 1
                FROM reachable r
                JOIN knowledge_edges e ON (e.source_id = r.nid OR e.target_id = r.nid)
                WHERE r.depth < ?2 {relation_clause}
            )
            SELECT DISTINCT n.id, n.label, n.node_type, n.content, n.confidence, n.created_at, n.updated_at
            FROM knowledge_nodes n
            JOIN reachable r ON n.id = r.nid
            WHERE n.id != ?1"
        );

        let mut stmt = db.prepare(&sql)?;
        let nodes = stmt
            .query_map(rusqlite::params![node_id, max_depth as i64], |row| {
                Ok(KnowledgeNode {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    node_type: row.get(2)?,
                    content: row.get(3)?,
                    confidence: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(nodes)
    }

    pub async fn update_node(
        &self,
        id: i64,
        content: Option<&str>,
        confidence: Option<f64>,
    ) -> Result<()> {
        let db = self.db.lock().await;
        if let Some(c) = content {
            db.execute(
                "UPDATE knowledge_nodes SET content = ?1, updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![c, id],
            )?;
        }
        if let Some(conf) = confidence {
            db.execute(
                "UPDATE knowledge_nodes SET confidence = ?1, updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![conf, id],
            )?;
        }
        Ok(())
    }

    pub async fn remove_node(&self, id: i64) -> Result<()> {
        let db = self.db.lock().await;
        db.execute("DELETE FROM knowledge_nodes WHERE id = ?1", [id])?;
        Ok(())
    }

    pub async fn remove_edge(&self, id: i64) -> Result<()> {
        let db = self.db.lock().await;
        db.execute("DELETE FROM knowledge_edges WHERE id = ?1", [id])?;
        Ok(())
    }

    pub async fn stats(&self) -> Result<(i64, i64)> {
        let db = self.db_read.lock().await;
        let nodes: i64 = db.query_row("SELECT COUNT(*) FROM knowledge_nodes", [], |r| r.get(0))?;
        let edges: i64 = db.query_row("SELECT COUNT(*) FROM knowledge_edges", [], |r| r.get(0))?;
        Ok((nodes, edges))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    #[tokio::test]
    async fn add_and_get_node() {
        let db = test_db();
        let db_read = db.clone();
        let kg = KnowledgeGraph::new(db, db_read);
        let id = kg.add_node("Rust", "language", "Systems programming lang", 0.9).await.unwrap();
        assert!(id > 0);
        let node = kg.get_node(id).await.unwrap();
        assert_eq!(node.label, "Rust");
        assert_eq!(node.node_type, "language");
        assert_eq!(node.content, "Systems programming lang");
        assert!((node.confidence - 0.9).abs() < 0.001);
    }

    #[tokio::test]
    async fn add_edge_and_neighbors() {
        let db = test_db();
        let db_read = db.clone();
        let kg = KnowledgeGraph::new(db, db_read);
        let n1 = kg.add_node("Rust", "lang", "", 1.0).await.unwrap();
        let n2 = kg.add_node("Cargo", "tool", "", 1.0).await.unwrap();
        let eid = kg.add_edge(n1, n2, "uses", 1.0).await.unwrap();
        assert!(eid > 0);
        let neighbors = kg.neighbors(n1, None).await.unwrap();
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].0.relation, "uses");
        assert_eq!(neighbors[0].1.label, "Cargo");
    }

    #[tokio::test]
    async fn neighbors_with_relation_filter() {
        let db = test_db();
        let db_read = db.clone();
        let kg = KnowledgeGraph::new(db, db_read);
        let n1 = kg.add_node("A", "t", "", 1.0).await.unwrap();
        let n2 = kg.add_node("B", "t", "", 1.0).await.unwrap();
        let n3 = kg.add_node("C", "t", "", 1.0).await.unwrap();
        kg.add_edge(n1, n2, "likes", 1.0).await.unwrap();
        kg.add_edge(n1, n3, "hates", 1.0).await.unwrap();
        let likes = kg.neighbors(n1, Some("likes")).await.unwrap();
        assert_eq!(likes.len(), 1);
        assert_eq!(likes[0].1.label, "B");
    }

    #[tokio::test]
    async fn search_finds_by_label() {
        let db = test_db();
        let db_read = db.clone();
        let kg = KnowledgeGraph::new(db, db_read);
        kg.add_node("Tokio runtime", "library", "Async runtime for Rust", 1.0).await.unwrap();
        kg.add_node("Axum web", "library", "Web framework", 1.0).await.unwrap();
        let results = kg.search("Tokio", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label, "Tokio runtime");
    }

    #[tokio::test]
    async fn stats_counts() {
        let db = test_db();
        let db_read = db.clone();
        let kg = KnowledgeGraph::new(db, db_read);
        let (n0, e0) = kg.stats().await.unwrap();
        assert_eq!(n0, 0);
        assert_eq!(e0, 0);
        let a = kg.add_node("A", "t", "", 1.0).await.unwrap();
        let b = kg.add_node("B", "t", "", 1.0).await.unwrap();
        kg.add_edge(a, b, "rel", 1.0).await.unwrap();
        let (n1, e1) = kg.stats().await.unwrap();
        assert_eq!(n1, 2);
        assert_eq!(e1, 1);
    }

    #[tokio::test]
    async fn get_node_nonexistent_errors() {
        let db = test_db();
        let db_read = db.clone();
        let kg = KnowledgeGraph::new(db, db_read);
        assert!(kg.get_node(9999).await.is_err());
    }

    #[tokio::test]
    async fn duplicate_edge_is_ignored() {
        let db = test_db();
        let db_read = db.clone();
        let kg = KnowledgeGraph::new(db, db_read);
        let a = kg.add_node("A", "t", "", 1.0).await.unwrap();
        let b = kg.add_node("B", "t", "", 1.0).await.unwrap();
        kg.add_edge(a, b, "rel", 1.0).await.unwrap();
        // INSERT OR IGNORE means duplicate edges don't error
        let r = kg.add_edge(a, b, "rel", 1.0).await;
        assert!(r.is_ok());
        // But only one edge should exist
        let (_, edge_count) = kg.stats().await.unwrap();
        assert_eq!(edge_count, 1);
    }
}

fn map_edge_node(row: &rusqlite::Row) -> rusqlite::Result<(KnowledgeEdge, KnowledgeNode)> {
    let metadata_str: String = row.get(5)?;
    let metadata = serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Object(Default::default()));
    Ok((
        KnowledgeEdge {
            id: row.get(0)?,
            source_id: row.get(1)?,
            target_id: row.get(2)?,
            relation: row.get(3)?,
            weight: row.get(4)?,
            metadata,
            created_at: row.get(6)?,
        },
        KnowledgeNode {
            id: row.get(7)?,
            label: row.get(8)?,
            node_type: row.get(9)?,
            content: row.get(10)?,
            confidence: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
        },
    ))
}
