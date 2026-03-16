use std::sync::Arc;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Persona {
    pub id: String,
    pub name: String,
    pub personality: String,
    /// Comma-separated tool names. Empty = all tools available.
    pub tools: String,
    pub created_at: String,
}

impl Persona {
    /// Returns the list of allowed tool names, or None if all tools are allowed.
    pub fn allowed_tools(&self) -> Option<Vec<&str>> {
        if self.tools.is_empty() {
            None
        } else {
            Some(self.tools.split(',').map(|s| s.trim()).collect())
        }
    }
}

/// Default specialist personas seeded on first run.
pub fn default_personas() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "coder",
            "Software Engineer",
            "You are an expert software engineer. Focus on writing clean, \
             efficient, well-tested code. Explain technical decisions clearly. \
             Prefer simple solutions over complex ones. When reviewing code, \
             look for bugs, security issues, and performance problems.",
        ),
        (
            "researcher",
            "Research Analyst",
            "You are a meticulous research analyst. Gather information \
             systematically, verify claims from multiple sources, and present \
             findings with clear citations. Distinguish between established \
             facts and speculation. Summarize key takeaways concisely.",
        ),
        (
            "writer",
            "Writing Specialist",
            "You are a skilled writer and editor. Focus on clarity, \
             conciseness, and appropriate tone for the audience. Structure \
             content logically with clear headings and transitions. Proofread \
             carefully and suggest improvements to existing text.",
        ),
        (
            "planner",
            "Strategic Planner",
            "You are a strategic planning specialist. Break complex objectives \
             into actionable steps with clear dependencies and priorities. \
             Identify risks and propose mitigations. Consider resource \
             constraints and timelines. Produce plans that are specific, \
             measurable, and achievable.",
        ),
    ]
}

/// Seed default personas if the table is empty.
pub async fn seed_defaults(db: &Arc<Mutex<Connection>>) -> Result<()> {
    let db = db.lock().await;
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM personas", [], |row| row.get(0))?;

    if count > 0 {
        return Ok(());
    }

    for (id, name, personality) in default_personas() {
        db.execute(
            "INSERT OR IGNORE INTO personas (id, name, personality) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, name, personality],
        )?;
    }

    info!(count = default_personas().len(), "seeded default personas");
    Ok(())
}

/// Get a persona by ID. Returns the default persona if not found.
pub async fn get_persona(
    db: &Arc<Mutex<Connection>>,
    id: &str,
    fallback_personality: &str,
) -> Persona {
    let db = db.lock().await;
    db.query_row(
        "SELECT id, name, personality, tools, created_at FROM personas WHERE id = ?1",
        [id],
        |row| {
            Ok(Persona {
                id: row.get(0)?,
                name: row.get(1)?,
                personality: row.get(2)?,
                tools: row.get(3)?,
                created_at: row.get(4)?,
            })
        },
    )
    .unwrap_or(Persona {
        id: "default".to_string(),
        name: "Default".to_string(),
        personality: if fallback_personality.is_empty() {
            "You are a helpful AI assistant.".to_string()
        } else {
            fallback_personality.to_string()
        },
        tools: String::new(),
        created_at: String::new(),
    })
}

/// List all personas.
pub async fn list_personas(db: &Arc<Mutex<Connection>>) -> Result<Vec<Persona>> {
    let db = db.lock().await;
    let mut stmt = db
        .prepare("SELECT id, name, personality, tools, created_at FROM personas ORDER BY id")?;

    let personas = stmt
        .query_map([], |row| {
            Ok(Persona {
                id: row.get(0)?,
                name: row.get(1)?,
                personality: row.get(2)?,
                tools: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(personas)
}
