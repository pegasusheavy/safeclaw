pub mod archival;
pub mod consolidation;
pub mod conversation;
pub mod core;
pub mod embeddings;
pub mod episodic;
pub mod extraction;
pub mod knowledge;
pub mod user_model;

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::error::Result;

pub struct MemoryManager {
    pub core: core::CoreMemory,
    pub conversation: conversation::ConversationMemory,
    pub archival: archival::ArchivalMemory,
    pub episodic: episodic::EpisodicMemory,
    pub user_model: user_model::UserModel,
    pub embeddings: Option<embeddings::EmbeddingEngine>,
    db: Arc<Mutex<Connection>>,
    db_read: Arc<Mutex<Connection>>,
}

impl MemoryManager {
    pub fn new(
        db: Arc<Mutex<Connection>>,
        db_read: Arc<Mutex<Connection>>,
        conversation_window: usize,
    ) -> Self {
        Self {
            core: core::CoreMemory::new(db.clone()),
            conversation: conversation::ConversationMemory::new(db.clone(), conversation_window),
            archival: archival::ArchivalMemory::new(db_read.clone()),
            episodic: episodic::EpisodicMemory::new(db.clone()),
            user_model: user_model::UserModel::new(db.clone()),
            embeddings: None,
            db,
            db_read,
        }
    }

    /// Initialize the embedding engine from memory config.
    pub fn init_embeddings(&mut self, ollama_host: &str, model: &str) {
        self.embeddings = embeddings::EmbeddingEngine::new(self.db.clone(), ollama_host, model);
        if self.embeddings.is_some() {
            tracing::info!(model, "embedding engine initialized");
        }
    }

    /// Semantic search over archival memory.
    /// Falls back to FTS5 if embeddings are unavailable or fail.
    pub async fn semantic_search_archival(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<archival::ArchivalEntry>> {
        if let Some(ref engine) = self.embeddings {
            match engine.search(query, "archival_memory", limit).await {
                Ok(results) if !results.is_empty() => {
                    let db = self.db_read.lock().await;
                    let mut entries = Vec::new();
                    for sr in &results {
                        if let Ok(entry) = db.query_row(
                            "SELECT id, content, category, created_at FROM archival_memory WHERE id = ?1",
                            [sr.source_id],
                            |row| {
                                Ok(archival::ArchivalEntry {
                                    id: row.get(0)?,
                                    content: row.get(1)?,
                                    category: row.get(2)?,
                                    created_at: row.get(3)?,
                                })
                            },
                        ) {
                            entries.push(entry);
                        }
                    }
                    if !entries.is_empty() {
                        return Ok(entries);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(err = %e, "embedding search failed, falling back to FTS5");
                }
            }
        }

        self.archival.search(query, limit).await
    }

    /// Semantic search over knowledge graph nodes.
    /// Falls back to FTS5 if embeddings are unavailable or fail.
    pub async fn semantic_search_knowledge(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<knowledge::KnowledgeNode>> {
        let kg = knowledge::KnowledgeGraph::new(self.db.clone(), self.db_read.clone());

        if let Some(ref engine) = self.embeddings {
            match engine.search(query, "knowledge_nodes", limit).await {
                Ok(results) if !results.is_empty() => {
                    let mut nodes = Vec::new();
                    for sr in &results {
                        if let Ok(node) = kg.get_node(sr.source_id).await {
                            nodes.push(node);
                        }
                    }
                    if !nodes.is_empty() {
                        return Ok(nodes);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(err = %e, "embedding search failed, falling back to FTS5");
                }
            }
        }

        kg.search(query, limit).await
    }

    /// Get the raw database handle (for extraction pipeline and consolidation).
    pub fn db(&self) -> Arc<Mutex<Connection>> {
        self.db.clone()
    }

    /// Initialize memory with config defaults.
    pub async fn init(&self, personality: &str) -> Result<()> {
        self.core.init(personality).await
    }

    /// Record an agent tick in stats.
    pub async fn record_tick(&self) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "UPDATE agent_stats SET total_ticks = total_ticks + 1, last_tick_at = datetime('now') WHERE id = 1",
            [],
        )?;
        Ok(())
    }

    /// Record an executed action in stats.
    pub async fn record_action(&self) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "UPDATE agent_stats SET total_actions = total_actions + 1 WHERE id = 1",
            [],
        )?;
        Ok(())
    }

    /// Get agent stats.
    pub async fn get_stats(&self) -> Result<AgentStats> {
        let db = self.db_read.lock().await;
        let stats = db.query_row(
            "SELECT total_ticks, total_actions, total_approved, total_rejected, last_tick_at, started_at
             FROM agent_stats WHERE id = 1",
            [],
            |row| {
                Ok(AgentStats {
                    total_ticks: row.get(0)?,
                    total_actions: row.get(1)?,
                    total_approved: row.get(2)?,
                    total_rejected: row.get(3)?,
                    last_tick_at: row.get(4)?,
                    started_at: row.get(5)?,
                })
            },
        )?;
        Ok(stats)
    }

    /// Log an activity entry.
    pub async fn log_activity(
        &self,
        action_type: &str,
        summary: &str,
        detail: Option<&str>,
        status: &str,
    ) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO activity_log (action_type, summary, detail, status) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![action_type, summary, detail, status],
        )?;
        Ok(())
    }

    /// Get recent activity log entries.
    pub async fn recent_activity(&self, limit: usize, offset: usize) -> Result<Vec<ActivityEntry>> {
        let db = self.db_read.lock().await;
        let mut stmt = db.prepare(
            "SELECT id, action_type, summary, detail, status, created_at
             FROM activity_log ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let entries = stmt
            .query_map(rusqlite::params![limit as i64, offset as i64], |row| {
                Ok(ActivityEntry {
                    id: row.get(0)?,
                    action_type: row.get(1)?,
                    summary: row.get(2)?,
                    detail: row.get(3)?,
                    status: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentStats {
    pub total_ticks: i64,
    pub total_actions: i64,
    pub total_approved: i64,
    pub total_rejected: i64,
    pub last_tick_at: Option<String>,
    pub started_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActivityEntry {
    pub id: i64,
    pub action_type: String,
    pub summary: String,
    pub detail: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    fn make_manager() -> MemoryManager {
        let db = test_db();
        let db_read = db.clone();
        MemoryManager::new(db, db_read, 50)
    }

    #[tokio::test]
    async fn init_sets_personality() {
        let mm = make_manager();
        mm.init("Helpful assistant").await.unwrap();
        let p = mm.core.get().await.unwrap();
        assert_eq!(p, "Helpful assistant");
    }

    #[tokio::test]
    async fn record_tick_increments_stats() {
        let mm = make_manager();
        let s0 = mm.get_stats().await.unwrap();
        assert_eq!(s0.total_ticks, 0);
        mm.record_tick().await.unwrap();
        mm.record_tick().await.unwrap();
        let s1 = mm.get_stats().await.unwrap();
        assert_eq!(s1.total_ticks, 2);
        assert!(s1.last_tick_at.is_some());
    }

    #[tokio::test]
    async fn record_action_increments_stats() {
        let mm = make_manager();
        mm.record_action().await.unwrap();
        let s = mm.get_stats().await.unwrap();
        assert_eq!(s.total_actions, 1);
    }

    #[tokio::test]
    async fn log_activity_and_retrieve() {
        let mm = make_manager();
        mm.log_activity("test", "did something", Some("details here"), "ok").await.unwrap();
        mm.log_activity("test", "another", None, "error").await.unwrap();
        let entries = mm.recent_activity(10, 0).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "another");
        assert_eq!(entries[0].status, "error");
        assert_eq!(entries[1].summary, "did something");
        assert_eq!(entries[1].detail.as_deref(), Some("details here"));
    }

    #[tokio::test]
    async fn recent_activity_pagination() {
        let mm = make_manager();
        for i in 0..5 {
            mm.log_activity("t", &format!("entry {i}"), None, "ok").await.unwrap();
        }
        let page = mm.recent_activity(2, 2).await.unwrap();
        assert_eq!(page.len(), 2);
    }

    #[tokio::test]
    async fn get_stats_has_started_at() {
        let mm = make_manager();
        let s = mm.get_stats().await.unwrap();
        assert!(!s.started_at.is_empty());
    }

    #[tokio::test]
    async fn recent_activity_empty() {
        let mm = make_manager();
        let activity = mm.recent_activity(10, 0).await.unwrap();
        assert!(activity.is_empty());
    }
}
