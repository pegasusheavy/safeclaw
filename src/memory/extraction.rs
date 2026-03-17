//! Post-conversation extraction pipeline.
//!
//! After each conversation, uses a secondary LLM call to extract:
//! - Key facts → archival memory
//! - User preferences → user profile
//! - Entities & relations → knowledge graph
//!
//! Runs asynchronously so it doesn't block the user-facing reply.

use std::sync::Arc;

use rusqlite::Connection;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::llm::{GenerateContext, LlmEngine};
use crate::memory::episodic::{EpisodeAction, EpisodicMemory};
use crate::memory::knowledge::KnowledgeGraph;
use crate::memory::user_model::UserModel;

#[derive(Debug, Deserialize)]
struct ExtractionResult {
    #[serde(default)]
    facts: Vec<String>,
    #[serde(default)]
    user_preferences: Vec<PreferenceExtract>,
    #[serde(default)]
    entities: Vec<EntityExtract>,
    #[serde(default)]
    relations: Vec<RelationExtract>,
    #[serde(default)]
    episode_summary: String,
}

#[derive(Debug, Deserialize)]
struct PreferenceExtract {
    key: String,
    value: String,
    #[serde(default = "default_confidence")]
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct EntityExtract {
    label: String,
    #[serde(default, rename = "type")]
    entity_type: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct RelationExtract {
    source: String,
    target: String,
    relation: String,
}

fn default_confidence() -> f64 {
    0.8
}

const EXTRACTION_PROMPT: &str = r#"Analyze this conversation and extract structured information as JSON.

CONVERSATION:
{conversation}

Extract the following into a JSON object (NO markdown fences, ONLY raw JSON):
{
  "facts": ["list of notable facts, commitments, or information mentioned"],
  "user_preferences": [{"key": "category_name", "value": "observed preference", "confidence": 0.8}],
  "entities": [{"label": "entity name", "type": "person/org/tool/concept/location", "content": "brief description"}],
  "relations": [{"source": "entity_label", "target": "entity_label", "relation": "relationship type"}],
  "episode_summary": "one-sentence summary of what happened in this interaction"
}

Rules:
- Only extract genuinely new or notable information, not trivia.
- For user_preferences, use keys like: communication_style, interests, schedule_patterns, technical_expertise, preferred_tools, work_context.
- For entities, only extract proper nouns or significant concepts.
- Relations should only reference entities you extracted.
- Confidence 0.5-1.0 (higher = more certain).
- If nothing notable to extract, return empty arrays and an episode_summary.
- Return ONLY valid JSON, no explanation."#;

/// Run the extraction pipeline against the most recent conversation.
///
/// This is designed to be spawned as a background task after a conversation
/// completes so it doesn't block the user-facing response.
pub async fn extract_from_conversation(
    db: Arc<Mutex<Connection>>,
    llm: &LlmEngine,
    conversation: &str,
    user_id: Option<&str>,
    tool_actions: &[EpisodeAction],
) {
    let prompt = EXTRACTION_PROMPT.replace("{conversation}", conversation);

    let gen_ctx = GenerateContext {
        message: &prompt,
        tools: None,
        prompt_skills: &[],
        images: Vec::new(),
    };

    let response = match llm.generate(&gen_ctx).await {
        Ok(r) => r,
        Err(e) => {
            warn!(err = %e, "extraction LLM call failed");
            return;
        }
    };

    let extraction = match parse_extraction_response(&response) {
        Some(e) => e,
        None => {
            debug!("extraction produced no parseable output");
            return;
        }
    };

    let kg = KnowledgeGraph::new(db.clone(), db.clone());
    let user_model = UserModel::new(db.clone());
    let episodic = EpisodicMemory::new(db.clone());

    // 1. Store facts as archival memories
    if !extraction.facts.is_empty() {
        let db_lock = db.lock().await;
        for fact in &extraction.facts {
            if let Err(e) = db_lock.execute(
                "INSERT INTO archival_memory (content, category) VALUES (?1, 'auto_extracted')",
                [fact],
            ) {
                warn!(err = %e, "failed to store extracted fact");
            }
        }
        drop(db_lock);
        info!(count = extraction.facts.len(), "extracted facts stored in archival memory");
    }

    // 2. Update user profile
    for pref in &extraction.user_preferences {
        if let Err(e) = user_model
            .set(user_id, &pref.key, &pref.value, pref.confidence, "auto_extraction")
            .await
        {
            warn!(key = %pref.key, err = %e, "failed to update user profile");
        }
    }
    if !extraction.user_preferences.is_empty() {
        info!(
            count = extraction.user_preferences.len(),
            "user profile entries updated"
        );
    }

    // 3. Populate knowledge graph with entities and relations
    let mut entity_ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for entity in &extraction.entities {
        match kg
            .add_node(&entity.label, &entity.entity_type, &entity.content, 0.8)
            .await
        {
            Ok(id) => {
                entity_ids.insert(entity.label.clone(), id);
            }
            Err(e) => {
                warn!(label = %entity.label, err = %e, "failed to add KG node");
            }
        }
    }

    for rel in &extraction.relations {
        let source_id = entity_ids.get(&rel.source);
        let target_id = entity_ids.get(&rel.target);
        if let (Some(&src), Some(&tgt)) = (source_id, target_id) {
            if let Err(e) = kg.add_edge(src, tgt, &rel.relation, 1.0).await {
                warn!(
                    source = %rel.source,
                    target = %rel.target,
                    err = %e,
                    "failed to add KG edge"
                );
            }
        }
    }

    if !extraction.entities.is_empty() {
        info!(
            nodes = extraction.entities.len(),
            edges = extraction.relations.len(),
            "knowledge graph auto-populated"
        );
    }

    // 4. Record episode
    let summary = if extraction.episode_summary.is_empty() {
        "conversation"
    } else {
        &extraction.episode_summary
    };
    let outcome = if extraction.facts.is_empty() {
        "no notable facts".to_string()
    } else {
        format!("{} facts extracted", extraction.facts.len())
    };

    if let Err(e) = episodic
        .record("user_message", summary, tool_actions, &outcome, user_id)
        .await
    {
        warn!(err = %e, "failed to record episode");
    }
}

fn parse_extraction_response(response: &str) -> Option<ExtractionResult> {
    // Try parsing the whole response as JSON
    if let Ok(result) = serde_json::from_str::<ExtractionResult>(response) {
        return Some(result);
    }

    // Try extracting JSON from markdown fences
    let json_str = if let Some(start) = response.find("```json") {
        let after_fence = &response[start + 7..];
        if let Some(end) = after_fence.find("```") {
            &after_fence[..end]
        } else {
            after_fence
        }
    } else if let Some(start) = response.find("```") {
        let after_fence = &response[start + 3..];
        if let Some(end) = after_fence.find("```") {
            &after_fence[..end]
        } else {
            after_fence
        }
    } else if let Some(start) = response.find('{') {
        // Find the last closing brace
        if let Some(end) = response.rfind('}') {
            &response[start..=end]
        } else {
            return None;
        }
    } else {
        return None;
    };

    serde_json::from_str::<ExtractionResult>(json_str.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let json = r#"{
            "facts": ["user prefers Rust over Python"],
            "user_preferences": [{"key": "language", "value": "Rust", "confidence": 0.9}],
            "entities": [{"label": "Rust", "type": "language", "content": "Systems programming"}],
            "relations": [],
            "episode_summary": "discussed programming languages"
        }"#;
        let result = parse_extraction_response(json).unwrap();
        assert_eq!(result.facts.len(), 1);
        assert_eq!(result.user_preferences.len(), 1);
        assert_eq!(result.entities.len(), 1);
    }

    #[test]
    fn parse_fenced_json() {
        let response = "Here's the extraction:\n```json\n{\"facts\": [\"test fact\"], \"user_preferences\": [], \"entities\": [], \"relations\": [], \"episode_summary\": \"test\"}\n```\nDone.";
        let result = parse_extraction_response(response).unwrap();
        assert_eq!(result.facts.len(), 1);
    }

    #[test]
    fn parse_embedded_json() {
        let response = "The extraction: {\"facts\": [], \"user_preferences\": [], \"entities\": [], \"relations\": [], \"episode_summary\": \"nothing notable\"} end";
        let result = parse_extraction_response(response).unwrap();
        assert_eq!(result.episode_summary, "nothing notable");
    }

    #[test]
    fn parse_empty_extraction() {
        let json = r#"{"facts": [], "user_preferences": [], "entities": [], "relations": [], "episode_summary": ""}"#;
        let result = parse_extraction_response(json).unwrap();
        assert!(result.facts.is_empty());
    }

    #[test]
    fn parse_garbage_returns_none() {
        assert!(parse_extraction_response("not json at all").is_none());
    }

    #[test]
    fn parse_missing_fields_uses_defaults() {
        let json = r#"{"episode_summary": "test"}"#;
        let result = parse_extraction_response(json).unwrap();
        assert!(result.facts.is_empty());
        assert!(result.user_preferences.is_empty());
        assert_eq!(result.episode_summary, "test");
    }
}
