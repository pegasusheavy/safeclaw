//! Memory decay & consolidation.
//!
//! Periodically finds old, unconsolidated archival memories, groups them,
//! asks the LLM to summarize them, and replaces the originals with a single
//! consolidated entry. This keeps the archival memory manageable over time.

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::error::Result;
use crate::llm::{GenerateContext, LlmEngine};

/// Identifies archival memories older than `age_days` that haven't been
/// consolidated yet, groups up to `batch_size` of them, and asks the LLM
/// to produce a summary. The originals are then marked as consolidated and
/// the summary is inserted as a new archival entry.
///
/// Returns the number of memories that were consolidated (0 if nothing to do).
pub async fn consolidate_old_memories(
    db: Arc<Mutex<Connection>>,
    llm: &LlmEngine,
    age_days: u32,
    batch_size: usize,
) -> Result<usize> {
    if batch_size == 0 {
        return Ok(0);
    }

    // Find old unconsolidated memories
    let entries = {
        let db_lock = db.lock().await;
        let mut stmt = db_lock.prepare(
            "SELECT id, content, category, created_at FROM archival_memory
             WHERE consolidated = 0
               AND created_at < datetime('now', ?1)
             ORDER BY created_at ASC
             LIMIT ?2",
        )?;

        let age_modifier = format!("-{age_days} days");
        let rows: Vec<(i64, String, String, String)> = stmt
            .query_map(rusqlite::params![age_modifier, batch_size as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    if entries.is_empty() {
        debug!("no archival memories old enough to consolidate");
        return Ok(0);
    }

    let count = entries.len();
    info!(
        count,
        age_days,
        "found archival memories to consolidate"
    );

    // Build the consolidation prompt
    let mut memory_text = String::new();
    for (i, (id, content, category, created_at)) in entries.iter().enumerate() {
        let cat_suffix = if category.is_empty() {
            String::new()
        } else {
            format!(" [{}]", category)
        };
        memory_text.push_str(&format!(
            "{i}. ({created_at}{cat_suffix}) #{id}: {content}\n"
        ));
    }

    let prompt = format!(
        "You are consolidating old memory entries for a personal AI assistant.\n\n\
         Below are {count} archival memory entries. Produce a SINGLE concise summary \
         that preserves all important facts, preferences, and actionable information, \
         but removes redundancy and trivial details.\n\n\
         ENTRIES:\n{memory_text}\n\
         Write ONLY the consolidated summary (1-3 paragraphs). No preamble."
    );

    let gen_ctx = GenerateContext {
        message: &prompt,
        tools: None,
        prompt_skills: &[],
        images: Vec::new(),
    };

    let summary = match llm.generate(&gen_ctx).await {
        Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
        Ok(_) => {
            warn!("consolidation LLM returned empty summary, skipping");
            return Ok(0);
        }
        Err(e) => {
            warn!(err = %e, "consolidation LLM call failed");
            return Ok(0);
        }
    };

    // Insert the consolidated summary
    let ids: Vec<i64> = entries.iter().map(|(id, ..)| *id).collect();
    {
        let db_lock = db.lock().await;

        // Insert the new consolidated entry
        db_lock.execute(
            "INSERT INTO archival_memory (content, category, consolidated) VALUES (?1, 'consolidated', 0)",
            [&summary],
        )?;

        // Mark the originals as consolidated
        for id in &ids {
            db_lock.execute(
                "UPDATE archival_memory SET consolidated = 1 WHERE id = ?1",
                [id],
            )?;
        }
    }

    info!(
        consolidated = count,
        summary_len = summary.len(),
        "archival memories consolidated"
    );

    Ok(count)
}

/// Count the number of unconsolidated memories older than `age_days`.
pub async fn pending_consolidation_count(
    db: Arc<Mutex<Connection>>,
    age_days: u32,
) -> Result<i64> {
    let db_lock = db.lock().await;
    let age_modifier = format!("-{age_days} days");
    let count: i64 = db_lock.query_row(
        "SELECT COUNT(*) FROM archival_memory
         WHERE consolidated = 0
           AND created_at < datetime('now', ?1)",
        [&age_modifier],
        |row| row.get(0),
    )?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    #[tokio::test]
    async fn pending_count_empty_db() {
        let db = test_db();
        let count = pending_consolidation_count(db, 30).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn pending_count_with_recent_entries() {
        let db = test_db();
        {
            let lock = db.lock().await;
            lock.execute(
                "INSERT INTO archival_memory (content, category) VALUES ('recent fact', '')",
                [],
            )
            .unwrap();
        }
        // Recent entries (just inserted) shouldn't be pending for consolidation
        let count = pending_consolidation_count(db, 30).await.unwrap();
        assert_eq!(count, 0);
    }
}
