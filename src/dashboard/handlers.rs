use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use super::routes::DashState;
use crate::memory::knowledge::KnowledgeGraph;
use crate::skills::SkillManager;

#[derive(Serialize)]
pub struct StatusResponse {
    pub running: bool,
    pub paused: bool,
    pub agent_name: String,
    pub dashboard_bind: String,
    pub tick_interval_secs: u64,
    pub tools_count: usize,
}

#[derive(Serialize)]
pub struct ActionResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

#[derive(Deserialize)]
pub struct PaginationQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
}

// -- Status & Control ----------------------------------------------------

pub async fn get_status(State(state): State<DashState>) -> Json<StatusResponse> {
    Json(StatusResponse {
        running: true,
        paused: state.agent.is_paused(),
        agent_name: state.agent.config.agent_name.clone(),
        dashboard_bind: state.agent.config.dashboard_bind.clone(),
        tick_interval_secs: state.agent.config.tick_interval_secs,
        tools_count: state.agent.tools.len(),
    })
}

pub async fn get_stats(
    State(state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state
        .agent
        .memory
        .get_stats()
        .await
        .map(|stats| Json(serde_json::to_value(stats).unwrap()))
        .map_err(|e| {
            error!("stats: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub async fn pause_agent(State(state): State<DashState>) -> Json<ActionResponse> {
    state.agent.pause();
    state.agent.notify_update();
    Json(ActionResponse {
        ok: true,
        message: Some("agent paused".into()),
        count: None,
    })
}

pub async fn resume_agent(State(state): State<DashState>) -> Json<ActionResponse> {
    state.agent.resume();
    state.agent.notify_update();
    Json(ActionResponse {
        ok: true,
        message: Some("agent resumed".into()),
        count: None,
    })
}

pub async fn force_tick(
    State(state): State<DashState>,
) -> Result<Json<ActionResponse>, StatusCode> {
    state
        .agent
        .force_tick()
        .await
        .map(|_| {
            state.agent.notify_update();
            Json(ActionResponse {
                ok: true,
                message: Some("tick completed".into()),
                count: None,
            })
        })
        .map_err(|e| {
            error!("force tick: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

// -- Approval Queue ------------------------------------------------------

pub async fn get_pending(
    State(state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state
        .agent
        .approval_queue
        .list_pending()
        .await
        .map(|actions| Json(serde_json::to_value(actions).unwrap()))
        .map_err(|e| {
            error!("list pending: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub async fn approve_action(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    state
        .agent
        .approval_queue
        .approve(&id)
        .await
        .map(|_| {
            state.agent.notify_update();
            Json(ActionResponse {
                ok: true,
                message: Some(format!("approved {id}")),
                count: None,
            })
        })
        .map_err(|e| {
            error!("approve: {e}");
            StatusCode::BAD_REQUEST
        })
}

pub async fn reject_action(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    state
        .agent
        .approval_queue
        .reject(&id)
        .await
        .map(|_| {
            state.agent.notify_update();
            Json(ActionResponse {
                ok: true,
                message: Some(format!("rejected {id}")),
                count: None,
            })
        })
        .map_err(|e| {
            error!("reject: {e}");
            StatusCode::BAD_REQUEST
        })
}

pub async fn approve_all(
    State(state): State<DashState>,
) -> Result<Json<ActionResponse>, StatusCode> {
    state
        .agent
        .approval_queue
        .approve_all()
        .await
        .map(|count| {
            state.agent.notify_update();
            Json(ActionResponse {
                ok: true,
                message: None,
                count: Some(count),
            })
        })
        .map_err(|e| {
            error!("approve_all: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub async fn reject_all(
    State(state): State<DashState>,
) -> Result<Json<ActionResponse>, StatusCode> {
    state
        .agent
        .approval_queue
        .reject_all()
        .await
        .map(|count| {
            state.agent.notify_update();
            Json(ActionResponse {
                ok: true,
                message: None,
                count: Some(count),
            })
        })
        .map_err(|e| {
            error!("reject_all: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

// -- Activity ------------------------------------------------------------

pub async fn get_activity(
    State(state): State<DashState>,
    Query(params): Query<PaginationQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let limit = params.limit.unwrap_or(50);
    let offset = params.offset.unwrap_or(0);
    state
        .agent
        .memory
        .recent_activity(limit, offset)
        .await
        .map(|entries| Json(serde_json::to_value(entries).unwrap()))
        .map_err(|e| {
            error!("activity: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

// -- Memory --------------------------------------------------------------

pub async fn get_core_memory(
    State(state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state
        .agent
        .memory
        .core
        .get()
        .await
        .map(|personality| Json(serde_json::json!({ "personality": personality })))
        .map_err(|e| {
            error!("core memory: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub async fn get_conversation_memory(
    State(state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state
        .agent
        .memory
        .conversation
        .recent()
        .await
        .map(|messages| Json(serde_json::to_value(messages).unwrap()))
        .map_err(|e| {
            error!("conversation memory: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub async fn search_archival_memory(
    State(state): State<DashState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let query = params.q.unwrap_or_default();
    if query.is_empty() {
        return state
            .agent
            .memory
            .archival
            .list(0, 50)
            .await
            .map(|entries| Json(serde_json::to_value(entries).unwrap()))
            .map_err(|e| {
                error!("archival list: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            });
    }
    state
        .agent
        .memory
        .archival
        .search(&query, 50)
        .await
        .map(|entries| Json(serde_json::to_value(entries).unwrap()))
        .map_err(|e| {
            error!("archival search: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Deserialize)]
pub struct ConversationHistoryQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_history_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_history_limit() -> usize {
    50
}

/// Search and paginate ALL conversation history (not just the window).
pub async fn conversation_history(
    State(state): State<DashState>,
    Query(params): Query<ConversationHistoryQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = state.db.lock().await;

    let result = if params.q.is_empty() {
        let mut stmt = db
            .prepare(
                "SELECT id, role, content, user_id, created_at FROM conversation_history ORDER BY id DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| {
                error!("conversation history: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![params.limit as i64, params.offset as i64], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "role": row.get::<_, String>(1)?,
                    "content": row.get::<_, String>(2)?,
                    "user_id": row.get::<_, Option<String>>(3)?,
                    "created_at": row.get::<_, String>(4)?
                }))
            })
            .map_err(|e| {
                error!("conversation history: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .filter_map(|r| r.ok())
            .collect();

        let total: i64 = db
            .query_row("SELECT COUNT(*) FROM conversation_history", [], |r| r.get(0))
            .unwrap_or(0);

        serde_json::json!({ "messages": rows, "total": total })
    } else {
        let pattern = format!("%{}%", params.q);
        let mut stmt = db
            .prepare(
                "SELECT id, role, content, user_id, created_at FROM conversation_history WHERE content LIKE ?1 ORDER BY id DESC LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| {
                error!("conversation history: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        let rows: Vec<serde_json::Value> = stmt
            .query_map(
                rusqlite::params![&pattern, params.limit as i64, params.offset as i64],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, i64>(0)?,
                        "role": row.get::<_, String>(1)?,
                        "content": row.get::<_, String>(2)?,
                        "user_id": row.get::<_, Option<String>>(3)?,
                        "created_at": row.get::<_, String>(4)?
                    }))
                },
            )
            .map_err(|e| {
                error!("conversation history: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .filter_map(|r| r.ok())
            .collect();

        let total: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM conversation_history WHERE content LIKE ?1",
                rusqlite::params![&pattern],
                |r| r.get(0),
            )
            .unwrap_or(0);

        serde_json::json!({ "messages": rows, "total": total })
    };

    Ok(Json(result))
}

// -- Persona ---------------------------------------------------------------

/// Get the agent's core personality.
pub async fn get_persona(State(state): State<DashState>) -> Json<serde_json::Value> {
    let personality = state.agent.memory.core.get().await.unwrap_or_default();
    Json(serde_json::json!({ "personality": personality }))
}

#[derive(Deserialize)]
pub struct PersonaUpdate {
    pub personality: String,
}

/// Update the agent's core personality.
pub async fn update_persona(
    State(state): State<DashState>,
    Json(body): Json<PersonaUpdate>,
) -> Json<serde_json::Value> {
    let db = state.db.lock().await;
    match db.execute(
        "UPDATE core_memory SET personality = ?1, updated_at = datetime('now') WHERE id = 1",
        [&body.personality],
    ) {
        Ok(_) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

// -- Specialist Personas -------------------------------------------------

/// List all specialist personas.
pub async fn list_personas(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    match crate::agent::personas::list_personas(&state.db).await {
        Ok(personas) => Json(serde_json::json!({ "personas": personas })),
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

#[derive(Deserialize)]
pub struct CreatePersona {
    pub id: String,
    pub name: String,
    pub personality: String,
    #[serde(default)]
    pub tools: String,
}

/// Create a new specialist persona.
pub async fn create_persona(
    State(state): State<DashState>,
    Json(body): Json<CreatePersona>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let db = state.db.lock().await;
    match db.execute(
        "INSERT INTO personas (id, name, personality, tools) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![body.id, body.name, body.personality, body.tools],
    ) {
        Ok(_) => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "ok": true, "id": body.id })),
        )),
        Err(e) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
        )),
    }
}

/// Update an existing specialist persona.
pub async fn update_specialist_persona(
    State(state): State<DashState>,
    Path(id): Path<String>,
    Json(body): Json<CreatePersona>,
) -> Json<serde_json::Value> {
    let db = state.db.lock().await;
    match db.execute(
        "UPDATE personas SET name = ?1, personality = ?2, tools = ?3 WHERE id = ?4",
        rusqlite::params![body.name, body.personality, body.tools, id],
    ) {
        Ok(0) => Json(serde_json::json!({ "ok": false, "error": "not found" })),
        Ok(_) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// Delete a specialist persona.
pub async fn delete_persona(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let db = state.db.lock().await;
    match db.execute("DELETE FROM personas WHERE id = ?1", [&id]) {
        Ok(0) => Json(serde_json::json!({ "ok": false, "error": "not found" })),
        Ok(_) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

// -- Knowledge Graph -----------------------------------------------------

pub async fn get_knowledge_nodes(
    State(state): State<DashState>,
    Query(params): Query<PaginationQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let limit = params.limit.unwrap_or(50) as i64;
    let offset = params.offset.unwrap_or(0) as i64;
    let db = state.db.lock().await;
    let mut stmt = db
        .prepare(
            "SELECT id, label, node_type, content, confidence, created_at, updated_at
             FROM knowledge_nodes ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
        )
        .map_err(|e| {
            error!("knowledge nodes: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let nodes: Vec<serde_json::Value> = stmt
        .query_map(rusqlite::params![limit, offset], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "label": row.get::<_, String>(1)?,
                "node_type": row.get::<_, String>(2)?,
                "content": row.get::<_, String>(3)?,
                "confidence": row.get::<_, f64>(4)?,
                "created_at": row.get::<_, String>(5)?,
                "updated_at": row.get::<_, String>(6)?,
            }))
        })
        .map_err(|e| {
            error!("knowledge nodes: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(serde_json::to_value(nodes).unwrap()))
}

pub async fn get_knowledge_node(
    State(state): State<DashState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let kg = KnowledgeGraph::new(state.db.clone(), state.db_read.clone());
    let node = kg.get_node(id).await.map_err(|e| {
        error!("knowledge node {id}: {e}");
        StatusCode::NOT_FOUND
    })?;
    Ok(Json(serde_json::to_value(node).unwrap()))
}

pub async fn get_knowledge_neighbors(
    State(state): State<DashState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let kg = KnowledgeGraph::new(state.db.clone(), state.db_read.clone());
    let neighbors = kg.neighbors(id, None).await.map_err(|e| {
        error!("knowledge neighbors: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let result: Vec<serde_json::Value> = neighbors
        .iter()
        .map(|(edge, node)| {
            serde_json::json!({
                "edge": {
                    "id": edge.id,
                    "relation": edge.relation,
                    "weight": edge.weight,
                    "source_id": edge.source_id,
                    "target_id": edge.target_id,
                },
                "node": {
                    "id": node.id,
                    "label": node.label,
                    "node_type": node.node_type,
                    "confidence": node.confidence,
                }
            })
        })
        .collect();
    Ok(Json(serde_json::to_value(result).unwrap()))
}

pub async fn search_knowledge(
    State(state): State<DashState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let query = params.q.unwrap_or_default();
    if query.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let kg = KnowledgeGraph::new(state.db.clone(), state.db_read.clone());
    let nodes = kg.search(&query, 50).await.map_err(|e| {
        error!("knowledge search: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(serde_json::to_value(nodes).unwrap()))
}

pub async fn get_knowledge_stats(
    State(state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let kg = KnowledgeGraph::new(state.db.clone(), state.db_read.clone());
    let (nodes, edges) = kg.stats().await.map_err(|e| {
        error!("knowledge stats: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(serde_json::json!({ "nodes": nodes, "edges": edges })))
}

// -- Tools ---------------------------------------------------------------

pub async fn list_tools(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let tools: Vec<serde_json::Value> = state
        .agent
        .tools
        .list()
        .iter()
        .map(|(name, desc)| {
            serde_json::json!({
                "name": name,
                "description": desc,
            })
        })
        .collect();
    Json(serde_json::to_value(tools).unwrap())
}

// -- Skills & Credentials ------------------------------------------------

pub async fn list_skills(
    State(state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let (skills_dir, running_info, credentials, manually_stopped, health_map) = {
        let sm = state.agent.skill_manager.lock().await;
        sm.list_data()
    };
    let skills =
        SkillManager::list_async(skills_dir, running_info, credentials, manually_stopped, health_map).await;
    Ok(Json(serde_json::to_value(skills).unwrap()))
}

#[derive(Deserialize)]
pub struct SetCredentialBody {
    pub key: String,
    pub value: String,
}

pub async fn get_skill_credentials(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    let creds = sm.get_credentials(&skill_name);
    // Return keys + whether they have values, but never expose raw secret values
    let masked: Vec<serde_json::Value> = creds
        .keys()
        .map(|k| {
            serde_json::json!({
                "key": k,
                "has_value": true,
            })
        })
        .collect();
    Ok(Json(serde_json::to_value(masked).unwrap()))
}

pub async fn set_skill_credential(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
    Json(body): Json<SetCredentialBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let mut sm = state.agent.skill_manager.lock().await;
    sm.set_credential(&skill_name, &body.key, &body.value)
        .map_err(|e| {
            error!("set credential: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("credential '{}' set for '{}'", body.key, skill_name)),
        count: None,
    }))
}

pub async fn delete_skill_credential(
    State(state): State<DashState>,
    Path((skill_name, key)): Path<(String, String)>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let mut sm = state.agent.skill_manager.lock().await;
    sm.delete_credential(&skill_name, &key)
        .map_err(|e| {
            error!("delete credential: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("credential '{}' removed from '{}'", key, skill_name)),
        count: None,
    }))
}

pub async fn restart_skill(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let mut sm = state.agent.skill_manager.lock().await;
    sm.restart_skill_by_name(&skill_name).await.map_err(|e| {
        error!("restart skill: {e}");
        StatusCode::NOT_FOUND
    })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("skill '{}' restarted", skill_name)),
        count: None,
    }))
}

/// List version snapshots for a skill.
pub async fn list_skill_versions(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    let versions = sm.list_versions(&skill_name).map_err(|e| {
        error!("list versions: {e}");
        StatusCode::NOT_FOUND
    })?;
    Ok(Json(serde_json::json!({ "versions": versions })))
}

/// Create a version snapshot of the current skill state.
pub async fn snapshot_skill_version(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    let version = sm.snapshot_version(&skill_name).map_err(|e| {
        error!("snapshot version: {e}");
        StatusCode::NOT_FOUND
    })?;
    Ok(Json(serde_json::json!({ "ok": true, "version": version })))
}

#[derive(Deserialize)]
pub struct RollbackBody {
    pub version: String,
}

/// Rollback a skill to a previous version snapshot.
pub async fn rollback_skill_version(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
    Json(body): Json<RollbackBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let mut sm = state.agent.skill_manager.lock().await;
    sm.rollback_version(&skill_name, &body.version).await.map_err(|e| {
        error!("rollback: {e}");
        StatusCode::NOT_FOUND
    })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("skill '{}' rolled back to {}", skill_name, body.version)),
        count: None,
    }))
}

/// Stop (kill) a running skill.  The skill will not be auto-restarted
/// by the reconcile loop until explicitly started again via the API.
pub async fn stop_skill(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let mut sm = state.agent.skill_manager.lock().await;
    sm.stop_skill_manual(&skill_name).await;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("skill '{}' stopped", skill_name)),
        count: None,
    }))
}

/// Start a skill that is currently stopped.  Clears any manual-stop
/// flag and launches the process.
pub async fn start_skill(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let mut sm = state.agent.skill_manager.lock().await;
    match sm.start_skill_by_name(&skill_name).await {
        Ok(true) => Ok(Json(ActionResponse {
            ok: true,
            message: Some(format!("skill '{}' started", skill_name)),
            count: None,
        })),
        Ok(false) => Ok(Json(ActionResponse {
            ok: true,
            message: Some(format!("skill '{}' is already running", skill_name)),
            count: None,
        })),
        Err(e) => {
            error!("start skill: {e}");
            Ok(Json(ActionResponse {
                ok: false,
                message: Some(format!("{e}")),
                count: None,
            }))
        }
    }
}

/// Get detailed information about a skill (manifest, env, logs).
pub async fn get_skill_detail(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    sm.detail(&skill_name)
        .map(|d| Json(serde_json::to_value(d).unwrap()))
        .map_err(|e| {
            error!("skill detail: {e}");
            StatusCode::NOT_FOUND
        })
}

/// Get skill log tail.
#[derive(Deserialize)]
pub struct LogQuery {
    pub lines: Option<usize>,
}

pub async fn get_skill_log(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
    Query(params): Query<LogQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    let max_lines = params.lines.unwrap_or(200);
    sm.read_log(&skill_name, max_lines)
        .map(|log| Json(serde_json::json!({ "log": log })))
        .map_err(|e| {
            error!("skill log: {e}");
            StatusCode::NOT_FOUND
        })
}

/// Update the skill manifest (raw TOML).
#[derive(Deserialize)]
pub struct UpdateManifestBody {
    pub toml: String,
}

pub async fn update_skill_manifest(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
    Json(body): Json<UpdateManifestBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    sm.update_manifest(&skill_name, &body.toml)
        .map_err(|e| {
            error!("update manifest: {e}");
            StatusCode::BAD_REQUEST
        })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("manifest for '{}' updated", skill_name)),
        count: None,
    }))
}

/// Toggle skill enabled/disabled.
#[derive(Deserialize)]
pub struct SetEnabledBody {
    pub enabled: bool,
}

pub async fn set_skill_enabled(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
    Json(body): Json<SetEnabledBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    {
        let sm = state.agent.skill_manager.lock().await;
        sm.set_enabled(&skill_name, body.enabled).map_err(|e| {
            error!("set enabled: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    // If disabling, stop the skill immediately; if enabling, reconcile to start it
    let mut sm = state.agent.skill_manager.lock().await;
    if !body.enabled {
        sm.stop_skill(&skill_name).await;
    } else {
        let _ = sm.reconcile().await;
    }

    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!(
            "skill '{}' {}",
            skill_name,
            if body.enabled { "enabled" } else { "disabled" }
        )),
        count: None,
    }))
}

/// Set an env var on a skill's manifest.
#[derive(Deserialize)]
pub struct SetEnvVarBody {
    pub key: String,
    pub value: String,
}

pub async fn set_skill_env_var(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
    Json(body): Json<SetEnvVarBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    sm.set_env_var(&skill_name, &body.key, &body.value)
        .map_err(|e| {
            error!("set env var: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("env var '{}' set for '{}'", body.key, skill_name)),
        count: None,
    }))
}

/// Delete an env var from a skill's manifest.
pub async fn delete_skill_env_var(
    State(state): State<DashState>,
    Path((skill_name, key)): Path<(String, String)>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    sm.delete_env_var(&skill_name, &key)
        .map_err(|e| {
            error!("delete env var: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("env var '{}' removed from '{}'", key, skill_name)),
        count: None,
    }))
}

// -- Skill Import / Delete -----------------------------------------------

#[derive(Deserialize)]
pub struct ImportSkillBody {
    /// Import source type: "git", "path", or "url".
    pub source: String,
    /// The git URL, local path, or archive URL.
    pub location: String,
    /// Optional skill name override (directory name).
    pub name: Option<String>,
}

pub async fn import_skill(
    State(state): State<DashState>,
    Json(body): Json<ImportSkillBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let sm = state.agent.skill_manager.lock().await;
    let name_ref = body.name.as_deref();

    match sm.import_skill(&body.source, &body.location, name_ref).await {
        Ok((name, _dir)) => {
            // Trigger reconcile to auto-start if enabled
            drop(sm);
            let mut sm = state.agent.skill_manager.lock().await;
            let _ = sm.reconcile().await;

            Ok(Json(ActionResponse {
                ok: true,
                message: Some(format!("skill '{}' imported successfully", name)),
                count: None,
            }))
        }
        Err(e) => {
            error!("import skill: {e}");
            Ok(Json(ActionResponse {
                ok: false,
                message: Some(format!("{e}")),
                count: None,
            }))
        }
    }
}

pub async fn delete_skill(
    State(state): State<DashState>,
    Path(skill_name): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let mut sm = state.agent.skill_manager.lock().await;
    sm.delete_skill(&skill_name)
        .await
        .map_err(|e| {
            error!("delete skill: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("skill '{}' deleted", skill_name)),
        count: None,
    }))
}

// -- Chat ----------------------------------------------------------------

#[derive(Deserialize)]
pub struct ChatAttachment {
    /// Base64-encoded file data (without data URI prefix).
    pub data: String,
    /// MIME type (e.g. "image/png", "audio/ogg", "application/pdf").
    pub mime_type: String,
    /// Original filename.
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Deserialize)]
pub struct ChatMessageBody {
    pub message: String,
    /// Optional user ID for multi-user routing.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Optional file attachments (images, audio, documents).
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
}

#[derive(Serialize)]
pub struct ChatResponse {
    pub reply: String,
    pub timestamp: String,
}

pub async fn send_chat_message(
    State(state): State<DashState>,
    Json(body): Json<ChatMessageBody>,
) -> Result<Json<ChatResponse>, StatusCode> {
    let mut message = body.message.trim().to_string();

    // Augment the message with attachment metadata so the agent knows
    // what the user sent and can invoke the appropriate tools.
    for att in &body.attachments {
        let fname = att.filename.as_deref().unwrap_or("attachment");
        let mime = &att.mime_type;
        if mime.starts_with("image/") {
            message.push_str(&format!(
                "\n[User attached an image: {fname} ({mime}). \
                 Use the 'image' tool to analyze it.]"
            ));
        } else if mime.starts_with("audio/") {
            message.push_str(&format!(
                "\n[User attached audio: {fname} ({mime}). \
                 Use the 'transcribe' tool on it.]"
            ));
        } else {
            message.push_str(&format!(
                "\n[User attached a document: {fname} ({mime}). \
                 Use the 'document' tool to extract its contents.]"
            ));
        }
    }

    if message.is_empty() && body.attachments.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Build user context if user_id is provided
    let user_ctx = if let Some(ref uid) = body.user_id {
        state.agent.user_manager.get_by_id(uid).await.ok()
            .map(|u| crate::users::UserContext::from_user(&u, "dashboard"))
    } else {
        None
    };

    let reply = state
        .agent
        .handle_message_as(&message, user_ctx.as_ref())
        .await
        .map_err(|e| {
            error!("chat: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let timestamp = chrono::Utc::now().to_rfc3339();

    Ok(Json(ChatResponse { reply, timestamp }))
}

// -- Tool Events (streaming progress) ------------------------------------

pub async fn get_tool_events(
    State(state): State<DashState>,
    Query(params): Query<PaginationQuery>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(50);
    let events = state.agent.recent_tool_events(limit).await;
    Json(serde_json::to_value(events).unwrap())
}

// -- Tunnel --------------------------------------------------------------

#[derive(Serialize)]
pub struct TunnelStatusResponse {
    pub enabled: bool,
    pub url: Option<String>,
}

pub async fn tunnel_status(
    State(state): State<DashState>,
) -> Json<TunnelStatusResponse> {
    let url = std::env::var("TUNNEL_URL").ok();
    Json(TunnelStatusResponse {
        enabled: state.config.tunnel.enabled || url.is_some(),
        url,
    })
}

// -- Trash ---------------------------------------------------------------

use crate::trash::{TrashEntry, TrashStats};

#[derive(Serialize)]
pub struct TrashListResponse {
    pub items: Vec<TrashEntry>,
    pub stats: TrashStats,
}

pub async fn list_trash(
    State(state): State<DashState>,
) -> Json<TrashListResponse> {
    let items = state.trash.list();
    let stats = state.trash.stats();
    Json(TrashListResponse { items, stats })
}

pub async fn trash_stats(
    State(state): State<DashState>,
) -> Json<TrashStats> {
    Json(state.trash.stats())
}

pub async fn restore_trash(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let entry = state.trash.restore(&id).map_err(|e| {
        error!("restore trash: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("Restored '{}' to {}", entry.name, entry.original_path)),
        count: None,
    }))
}

pub async fn permanent_delete_trash(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let entry = state.trash.permanent_delete(&id).map_err(|e| {
        error!("permanent delete trash: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("Permanently deleted '{}'", entry.name)),
        count: None,
    }))
}

pub async fn empty_trash(
    State(state): State<DashState>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let count = state.trash.empty().map_err(|e| {
        error!("empty trash: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("Emptied trash ({count} items removed)")),
        count: Some(count as u64),
    }))
}

// -- Goals ---------------------------------------------------------------

pub async fn list_goals(
    State(state): State<DashState>,
    Query(params): Query<GoalListQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use crate::goals::GoalManager;

    let mgr = GoalManager::new(state.db.clone());
    let status = params.status.as_deref();
    let limit = params.limit.unwrap_or(50);
    let offset = params.offset.unwrap_or(0);

    let goals = mgr
        .list_goals(status, limit, offset)
        .await
        .map_err(|e| {
            error!("list goals: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(serde_json::to_value(goals).unwrap()))
}

#[derive(Deserialize)]
pub struct GoalListQuery {
    pub status: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn get_goal(
    State(state): State<DashState>,
    Path(goal_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use crate::goals::GoalManager;

    let mgr = GoalManager::new(state.db.clone());
    let goal = mgr.get_goal(&goal_id).await.map_err(|e| {
        error!("get goal: {e}");
        StatusCode::NOT_FOUND
    })?;
    let tasks = mgr.get_tasks(&goal_id).await.map_err(|e| {
        error!("get goal tasks: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(serde_json::json!({
        "goal": goal,
        "tasks": tasks,
    })))
}

pub async fn update_goal_status(
    State(state): State<DashState>,
    Path(goal_id): Path<String>,
    Json(body): Json<UpdateGoalStatusBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    use crate::goals::{GoalManager, GoalStatus};

    let mgr = GoalManager::new(state.db.clone());
    let status = GoalStatus::from_str(&body.status);
    mgr.update_goal_status(&goal_id, status)
        .await
        .map_err(|e| {
            error!("update goal status: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("Goal {} status set to {}", goal_id, body.status)),
        count: None,
    }))
}

#[derive(Deserialize)]
pub struct UpdateGoalStatusBody {
    pub status: String,
}

// -- Security: Audit Trail ---------------------------------------------------

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub event_type: Option<String>,
    pub tool: Option<String>,
}

pub async fn get_audit_log(
    State(state): State<DashState>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let limit = query.limit.unwrap_or(50);
    let offset = query.offset.unwrap_or(0);
    let entries = state
        .agent
        .audit
        .recent(limit, offset, query.event_type.as_deref(), query.tool.as_deref())
        .await;
    Ok(Json(serde_json::to_value(entries).unwrap()))
}

pub async fn get_audit_summary(
    State(state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let summary = state.agent.audit.summary().await;
    Ok(Json(serde_json::to_value(summary).unwrap()))
}

pub async fn explain_action(
    State(state): State<DashState>,
    Path(audit_id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let chain = state.agent.audit.explain_action(audit_id).await;
    Ok(Json(serde_json::to_value(chain).unwrap()))
}

// -- Security: Cost Tracking -------------------------------------------------

pub async fn get_cost_summary(
    State(state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let summary = state.agent.cost_tracker.summary().await;
    Ok(Json(serde_json::to_value(summary).unwrap()))
}

pub async fn get_cost_recent(
    State(state): State<DashState>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let limit = query.limit.unwrap_or(50);
    let records = state.agent.cost_tracker.recent(limit).await;
    Ok(Json(serde_json::to_value(records).unwrap()))
}

// -- Security: Rate Limiting -------------------------------------------------

pub async fn get_rate_limit_status(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let status = state.agent.rate_limiter.status();
    Json(serde_json::json!({
        "calls_last_minute": status.calls_last_minute,
        "calls_last_hour": status.calls_last_hour,
        "limit_per_minute": status.limit_per_minute,
        "limit_per_hour": status.limit_per_hour,
        "is_limited": status.is_limited,
    }))
}

// -- Security: 2FA -----------------------------------------------------------

pub async fn get_2fa_challenges(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let challenges = state.agent.twofa.pending();
    Json(serde_json::to_value(challenges).unwrap())
}

pub async fn confirm_2fa(
    State(state): State<DashState>,
    Path(challenge_id): Path<String>,
) -> Json<ActionResponse> {
    let ok = state.agent.twofa.confirm(&challenge_id);
    if ok {
        state.agent.audit.log_2fa("", "confirmed", "dashboard").await;
    }
    Json(ActionResponse {
        ok,
        message: Some(if ok {
            "2FA challenge confirmed".into()
        } else {
            "Challenge not found or already resolved".into()
        }),
        count: None,
    })
}

pub async fn reject_2fa(
    State(state): State<DashState>,
    Path(challenge_id): Path<String>,
) -> Json<ActionResponse> {
    let ok = state.agent.twofa.reject(&challenge_id);
    if ok {
        state.agent.audit.log_2fa("", "rejected", "dashboard").await;
    }
    Json(ActionResponse {
        ok,
        message: Some(if ok {
            "2FA challenge rejected".into()
        } else {
            "Challenge not found".into()
        }),
        count: None,
    })
}

// -- Security: Overview (combined) -------------------------------------------

pub async fn get_security_overview(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let (audit_summary, cost_summary) = tokio::join!(
        state.agent.audit.summary(),
        state.agent.cost_tracker.summary(),
    );
    let rate_status = state.agent.rate_limiter.status();
    let twofa_pending = state.agent.twofa.pending();

    Json(serde_json::json!({
        "audit": audit_summary,
        "cost": cost_summary,
        "rate_limit": {
            "calls_last_minute": rate_status.calls_last_minute,
            "calls_last_hour": rate_status.calls_last_hour,
            "limit_per_minute": rate_status.limit_per_minute,
            "limit_per_hour": rate_status.limit_per_hour,
            "is_limited": rate_status.is_limited,
        },
        "twofa_pending": twofa_pending.len(),
        "blocked_tools": state.agent.config.security.blocked_tools,
        "pii_detection_enabled": state.agent.config.security.pii_detection,
    }))
}

// -- Health Check ------------------------------------------------------------

/// Health check endpoint for load balancers and monitoring.
/// Returns 200 OK with component health if the agent is running.
/// Returns 503 Service Unavailable if the agent is unhealthy.
pub async fn healthz(
    State(state): State<DashState>,
) -> impl IntoResponse {
    let db_ok = {
        let db = state.db.lock().await;
        db.execute_batch("SELECT 1").is_ok()
    };

    let agent_ok = !state.agent.is_paused();
    let healthy = db_ok; // DB is the critical component

    let body = serde_json::json!({
        "status": if healthy { "healthy" } else { "unhealthy" },
        "version": env!("CARGO_PKG_VERSION"),
        "checks": {
            "database": if db_ok { "ok" } else { "error" },
            "agent": if agent_ok { "running" } else { "paused" },
            "tools": state.agent.tools.len(),
        },
        "uptime_secs": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });

    if healthy {
        (StatusCode::OK, Json(body))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(body))
    }
}

// -- Prometheus Metrics -------------------------------------------------------

/// Prometheus-compatible metrics endpoint.
/// Exposes counters, gauges, and histograms in the OpenMetrics text format.
pub async fn metrics(
    State(state): State<DashState>,
) -> impl IntoResponse {
    let mut out = String::new();

    // Agent info
    out.push_str(&format!(
        "# HELP safeclaw_info Agent metadata.\n\
         # TYPE safeclaw_info gauge\n\
         safeclaw_info{{version=\"{}\",agent_name=\"{}\"}} 1\n\n",
        env!("CARGO_PKG_VERSION"),
        state.agent.config.agent_name,
    ));

    // Agent paused
    out.push_str(&format!(
        "# HELP safeclaw_paused Whether the agent is paused.\n\
         # TYPE safeclaw_paused gauge\n\
         safeclaw_paused {}\n\n",
        if state.agent.is_paused() { 1 } else { 0 },
    ));

    // Tools count
    out.push_str(&format!(
        "# HELP safeclaw_tools_registered Number of registered tools.\n\
         # TYPE safeclaw_tools_registered gauge\n\
         safeclaw_tools_registered {}\n\n",
        state.agent.tools.len(),
    ));

    // Fetch stats, audit, and cost in parallel (stats uses db_read, others use db)
    let (stats_result, audit, cost) = tokio::join!(
        state.agent.memory.get_stats(),
        state.agent.audit.summary(),
        state.agent.cost_tracker.summary(),
    );

    if let Ok(stats) = stats_result {
        out.push_str(&format!(
            "# HELP safeclaw_ticks_total Total agent ticks executed.\n\
             # TYPE safeclaw_ticks_total counter\n\
             safeclaw_ticks_total {}\n\n",
            stats.total_ticks,
        ));
        out.push_str(&format!(
            "# HELP safeclaw_actions_approved_total Total approved actions.\n\
             # TYPE safeclaw_actions_approved_total counter\n\
             safeclaw_actions_approved_total {}\n\n",
            stats.total_approved,
        ));
        out.push_str(&format!(
            "# HELP safeclaw_actions_rejected_total Total rejected actions.\n\
             # TYPE safeclaw_actions_rejected_total counter\n\
             safeclaw_actions_rejected_total {}\n\n",
            stats.total_rejected,
        ));
    }

    out.push_str(&format!(
        "# HELP safeclaw_audit_events_total Total audit log events.\n\
         # TYPE safeclaw_audit_events_total counter\n\
         safeclaw_audit_events_total {}\n\n",
        audit.total_events,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_tool_calls_total Total tool calls.\n\
         # TYPE safeclaw_tool_calls_total counter\n\
         safeclaw_tool_calls_total {}\n\n",
        audit.tool_calls,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_rate_limits_total Total rate limit events.\n\
         # TYPE safeclaw_rate_limits_total counter\n\
         safeclaw_rate_limits_total {}\n\n",
        audit.rate_limits,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_pii_detections_total Total PII detections.\n\
         # TYPE safeclaw_pii_detections_total counter\n\
         safeclaw_pii_detections_total {}\n\n",
        audit.pii_detections,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_llm_cost_today_usd Estimated LLM cost today in USD.\n\
         # TYPE safeclaw_llm_cost_today_usd gauge\n\
         safeclaw_llm_cost_today_usd {:.6}\n\n",
        cost.today_usd,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_llm_cost_total_usd Estimated LLM cost all-time in USD.\n\
         # TYPE safeclaw_llm_cost_total_usd counter\n\
         safeclaw_llm_cost_total_usd {:.6}\n\n",
        cost.total_usd,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_llm_tokens_today Total LLM tokens used today.\n\
         # TYPE safeclaw_llm_tokens_today gauge\n\
         safeclaw_llm_tokens_today {}\n\n",
        cost.today_tokens,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_llm_tokens_total Total LLM tokens used all-time.\n\
         # TYPE safeclaw_llm_tokens_total counter\n\
         safeclaw_llm_tokens_total {}\n\n",
        cost.total_tokens,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_llm_requests_today LLM requests today.\n\
         # TYPE safeclaw_llm_requests_today gauge\n\
         safeclaw_llm_requests_today {}\n\n",
        cost.today_requests,
    ));

    // Rate limiter
    let rate = state.agent.rate_limiter.status();
    out.push_str(&format!(
        "# HELP safeclaw_rate_calls_minute Tool calls in the last minute.\n\
         # TYPE safeclaw_rate_calls_minute gauge\n\
         safeclaw_rate_calls_minute {}\n\n",
        rate.calls_last_minute,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_rate_calls_hour Tool calls in the last hour.\n\
         # TYPE safeclaw_rate_calls_hour gauge\n\
         safeclaw_rate_calls_hour {}\n\n",
        rate.calls_last_hour,
    ));
    out.push_str(&format!(
        "# HELP safeclaw_rate_limited Whether the agent is currently rate limited.\n\
         # TYPE safeclaw_rate_limited gauge\n\
         safeclaw_rate_limited {}\n\n",
        if rate.is_limited { 1 } else { 0 },
    ));

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "text/plain; version=0.0.4; charset=utf-8".parse().unwrap(),
    );
    (headers, out)
}

// -- Backup & Restore --------------------------------------------------------

/// Create a backup archive (JSON dump of all data).
pub async fn create_backup(
    State(state): State<DashState>,
) -> Result<impl IntoResponse, StatusCode> {
    let db = state.db.lock().await;

    let backup = collect_backup_data(&db).map_err(|e| {
        error!("backup failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let json = serde_json::to_string_pretty(&backup).map_err(|e| {
        error!("backup serialization failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!(
            "attachment; filename=\"safeclaw-backup-{}.json\"",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        )
        .parse()
        .unwrap(),
    );

    Ok((headers, json))
}

/// Restore from a backup archive.
#[derive(Deserialize)]
pub struct RestoreBody {
    pub tables: serde_json::Value,
}

pub async fn restore_backup(
    State(state): State<DashState>,
    Json(body): Json<RestoreBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let db = state.db.lock().await;

    let tables = body.tables.as_object().ok_or_else(|| {
        error!("restore: 'tables' must be an object");
        StatusCode::BAD_REQUEST
    })?;

    let mut restored = 0u64;
    for (table_name, rows) in tables {
        let rows_arr = match rows.as_array() {
            Some(a) => a,
            None => continue,
        };

        // Only restore known safe tables
        let allowed = [
            "core_memory", "archival_memory", "activity_log",
            "cron_jobs", "goals", "goal_tasks",
        ];
        if !allowed.contains(&table_name.as_str()) {
            continue;
        }

        for row in rows_arr {
            if let Some(obj) = row.as_object() {
                let columns: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
                let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();

                let sql = format!(
                    "INSERT OR REPLACE INTO {} ({}) VALUES ({})",
                    table_name,
                    columns.join(", "),
                    placeholders.join(", "),
                );

                let values: Vec<String> = obj
                    .values()
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .collect();

                let params: Vec<&dyn rusqlite::types::ToSql> =
                    values.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

                if db.execute(&sql, params.as_slice()).is_ok() {
                    restored += 1;
                }
            }
        }
    }

    info!(rows = restored, "backup restored");

    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("Restored {restored} rows")),
        count: Some(restored),
    }))
}

fn collect_backup_data(
    db: &rusqlite::Connection,
) -> std::result::Result<serde_json::Value, String> {
    let tables = [
        "core_memory",
        "archival_memory",
        "activity_log",
        "cron_jobs",
        "goals",
        "goal_tasks",
        "agent_stats",
    ];

    let mut backup = serde_json::Map::new();
    backup.insert(
        "version".to_string(),
        serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    backup.insert(
        "created_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );

    let mut table_data = serde_json::Map::new();
    for table in &tables {
        let rows = dump_table(db, table).map_err(|e| format!("dump {table}: {e}"))?;
        table_data.insert(table.to_string(), serde_json::Value::Array(rows));
    }

    backup.insert("tables".to_string(), serde_json::Value::Object(table_data));
    Ok(serde_json::Value::Object(backup))
}

fn dump_table(
    db: &rusqlite::Connection,
    table: &str,
) -> std::result::Result<Vec<serde_json::Value>, rusqlite::Error> {
    let sql = format!("SELECT * FROM {table}");
    let mut stmt = db.prepare(&sql)?;
    let col_count = stmt.column_count();
    let col_names: Vec<String> = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
        .collect();

    let rows = stmt.query_map([], |row| {
        let mut obj = serde_json::Map::new();
        for (i, name) in col_names.iter().enumerate() {
            let val: rusqlite::types::Value = row.get(i)?;
            let json_val = match val {
                rusqlite::types::Value::Null => serde_json::Value::Null,
                rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                rusqlite::types::Value::Real(f) => serde_json::json!(f),
                rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
                rusqlite::types::Value::Blob(b) => {
                    serde_json::Value::String(format!("[blob:{}bytes]", b.len()))
                }
            };
            obj.insert(name.clone(), json_val);
        }
        Ok(serde_json::Value::Object(obj))
    })?;

    rows.collect()
}

// -- Auto-update mechanism ---------------------------------------------------

/// Check for available updates by comparing the current version
/// against the latest GitHub release.
pub async fn check_update(
    State(_state): State<DashState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let current = env!("CARGO_PKG_VERSION");

    // Check GitHub releases API
    let client = reqwest::Client::builder()
        .user_agent("safeclaw")
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let resp = client
        .get("https://api.github.com/repos/PegasusHeavyIndustries/safeclaw/releases/latest")
        .send()
        .await
        .map_err(|e| {
            error!("update check failed: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    if !resp.status().is_success() {
        return Ok(Json(serde_json::json!({
            "current_version": current,
            "update_available": false,
            "error": format!("GitHub API returned {}", resp.status()),
        })));
    }

    let release: serde_json::Value = resp.json().await.map_err(|e| {
        error!("update check parse failed: {e}");
        StatusCode::BAD_GATEWAY
    })?;

    let latest_tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let latest_version = latest_tag.strip_prefix('v').unwrap_or(latest_tag);
    let release_url = release
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let body = release
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let published = release
        .get("published_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let update_available = version_gt(latest_version, current);

    Ok(Json(serde_json::json!({
        "current_version": current,
        "latest_version": latest_version,
        "update_available": update_available,
        "release_url": release_url,
        "release_notes": body,
        "published_at": published,
    })))
}

/// Trigger an update: pull latest, rebuild, and restart.
/// Only works inside a Docker container or a git repo with cargo.
pub async fn trigger_update(
    State(_state): State<DashState>,
) -> Result<Json<ActionResponse>, StatusCode> {
    // Attempt git-based update
    let git_pull = tokio::process::Command::new("git")
        .args(["pull", "--ff-only"])
        .output()
        .await;

    match git_pull {
        Ok(output) if output.status.success() => {
            let msg = String::from_utf8_lossy(&output.stdout);
            info!(output = %msg, "git pull succeeded");

            // Try to rebuild
            let build = tokio::process::Command::new("cargo")
                .args(["build", "--release"])
                .output()
                .await;

            match build {
                Ok(bout) if bout.status.success() => {
                    info!("rebuild succeeded — restart to apply update");
                    Ok(Json(ActionResponse {
                        ok: true,
                        message: Some("Update pulled and rebuilt. Restart to apply.".into()),
                        count: None,
                    }))
                }
                Ok(bout) => {
                    let stderr = String::from_utf8_lossy(&bout.stderr);
                    error!(stderr = %stderr, "rebuild failed");
                    Ok(Json(ActionResponse {
                        ok: false,
                        message: Some(format!("Git pull OK but rebuild failed: {}", stderr.chars().take(200).collect::<String>())),
                        count: None,
                    }))
                }
                Err(e) => {
                    // cargo not available — Docker update path
                    info!("cargo not found ({e}), signaling for container restart");
                    Ok(Json(ActionResponse {
                        ok: true,
                        message: Some("Update pulled. Restart container to apply.".into()),
                        count: None,
                    }))
                }
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Ok(Json(ActionResponse {
                ok: false,
                message: Some(format!("git pull failed: {}", stderr.chars().take(200).collect::<String>())),
                count: None,
            }))
        }
        Err(e) => {
            Ok(Json(ActionResponse {
                ok: false,
                message: Some(format!("git not available: {e}")),
                count: None,
            }))
        }
    }
}

/// Simple semantic version comparison. Returns true if a > b.
pub(crate) fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        let parts: Vec<u32> = s.split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        (
            parts.first().copied().unwrap_or(0),
            parts.get(1).copied().unwrap_or(0),
            parts.get(2).copied().unwrap_or(0),
        )
    };
    parse(a) > parse(b)
}

// -- Federation --------------------------------------------------------------

/// Get federation status for this node.
pub async fn federation_status(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let info = state.agent.federation.local_info();
    let peers = state.agent.federation.list_peers().await;

    Json(serde_json::json!({
        "enabled": state.agent.federation.is_enabled(),
        "node": info,
        "peers": peers,
        "peer_count": peers.len(),
    }))
}

/// List all known federation peers.
pub async fn federation_peers(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let peers = state.agent.federation.list_peers().await;
    Json(serde_json::json!({ "peers": peers }))
}

/// Add a peer by address.
#[derive(Deserialize)]
pub struct AddPeerBody {
    pub address: String,
}

pub async fn federation_add_peer(
    State(state): State<DashState>,
    Json(body): Json<AddPeerBody>,
) -> Result<Json<ActionResponse>, StatusCode> {
    // Fetch the peer's info
    let client = reqwest::Client::builder()
        .user_agent("safeclaw-federation")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let url = format!("{}/healthz", body.address);
    let resp = client.get(&url).send().await.map_err(|e| {
        error!("failed to reach peer at {}: {e}", body.address);
        StatusCode::BAD_GATEWAY
    })?;

    if !resp.status().is_success() {
        return Ok(Json(ActionResponse {
            ok: false,
            message: Some(format!("Peer at {} is not reachable", body.address)),
            count: None,
        }));
    }

    let peer_info: serde_json::Value = resp.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let version = peer_info
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Register the peer with a fabricated NodeInfo
    let node_info = crate::federation::NodeInfo {
        node_id: uuid::Uuid::new_v4().to_string(),
        name: body.address.clone(),
        address: body.address.clone(),
        version: version.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        last_heartbeat: chrono::Utc::now().to_rfc3339(),
        status: crate::federation::NodeStatus::Online,
    };

    state.agent.federation.register_peer(node_info).await;

    // Register ourselves with the peer
    let register_url = format!("{}/api/federation/heartbeat", body.address);
    let local = state.agent.federation.local_info();
    let _ = client.post(&register_url).json(&local).send().await;

    Ok(Json(ActionResponse {
        ok: true,
        message: Some(format!("Peer {} added (version {version})", body.address)),
        count: None,
    }))
}

/// Remove a peer by node ID.
pub async fn federation_remove_peer(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Json<ActionResponse> {
    state.agent.federation.remove_peer(&id).await;
    Json(ActionResponse {
        ok: true,
        message: Some(format!("Peer {id} removed")),
        count: None,
    })
}

/// Receive sync deltas from a peer (no auth required).
#[derive(Deserialize)]
pub struct SyncBody {
    pub origin: String,
    pub deltas: Vec<crate::federation::MemoryDelta>,
}

pub async fn federation_receive_sync(
    State(state): State<DashState>,
    Json(body): Json<SyncBody>,
) -> Json<ActionResponse> {
    let count = body.deltas.len();
    state.agent.federation.apply_deltas(&state.db, body.deltas).await;
    Json(ActionResponse {
        ok: true,
        message: Some(format!("Applied {count} deltas from {}", body.origin)),
        count: Some(count as u64),
    })
}

/// Receive heartbeat from a peer (no auth required).
pub async fn federation_receive_heartbeat(
    State(state): State<DashState>,
    Json(info): Json<crate::federation::NodeInfo>,
) -> Json<ActionResponse> {
    state.agent.federation.register_peer(info).await;
    Json(ActionResponse {
        ok: true,
        message: Some("Heartbeat accepted".into()),
        count: None,
    })
}

/// Receive a task claim notification from a peer (no auth required).
pub async fn federation_receive_claim(
    State(_state): State<DashState>,
    Json(claim): Json<crate::federation::TaskClaim>,
) -> Json<ActionResponse> {
    info!(
        task_id = %claim.task_id,
        claimed_by = %claim.claimed_by,
        "received task claim from peer"
    );
    Json(ActionResponse {
        ok: true,
        message: Some(format!("Claim for {} acknowledged", claim.task_id)),
        count: None,
    })
}

// -- LLM Plugin Backend Management -------------------------------------------

/// List all registered LLM backends and which is active.
pub async fn llm_backends(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let available = state.agent.llm.available_backends();
    let active = state.agent.llm.active_backend();
    let info = state.agent.llm.backend_info();

    Json(serde_json::json!({
        "active": active,
        "active_info": info,
        "available": available,
    }))
}

// -- User Management ---------------------------------------------------------

/// List all users.
pub async fn list_users(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let users = state.agent.user_manager.list().await;
    Json(serde_json::json!({ "users": users }))
}

/// Get a single user by ID.
pub async fn get_user(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let user = state.agent.user_manager.get_by_id(&id).await.map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!(user)))
}

/// Create a new user.
#[derive(Deserialize)]
pub struct CreateUserBody {
    pub username: String,
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub password: Option<String>,
    pub email: Option<String>,
    pub telegram_id: Option<i64>,
    pub whatsapp_id: Option<String>,
}

pub async fn create_user(
    State(state): State<DashState>,
    Json(body): Json<CreateUserBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let role = body.role.as_deref().map(crate::users::UserRole::from_str)
        .unwrap_or(crate::users::UserRole::User);
    let display = body.display_name.as_deref().unwrap_or(&body.username);
    let password = body.password.as_deref().unwrap_or("");

    let user = state.agent.user_manager
        .create(&body.username, display, role, password)
        .await
        .map_err(|e| {
            error!("create user: {e}");
            StatusCode::CONFLICT
        })?;

    // Link platform IDs if provided
    if let Some(email) = &body.email {
        let _ = state.agent.user_manager.update(&user.id, None, None, Some(email), None).await;
    }
    if let Some(tg_id) = body.telegram_id {
        let _ = state.agent.user_manager.link_telegram(&user.id, tg_id).await;
    }
    if let Some(ref wa_id) = body.whatsapp_id {
        let _ = state.agent.user_manager.link_whatsapp(&user.id, wa_id).await;
    }

    let user = state.agent.user_manager.get_by_id(&user.id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!(user)))
}

/// Update a user.
#[derive(Deserialize)]
pub struct UpdateUserBody {
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub email: Option<String>,
    pub enabled: Option<bool>,
    pub telegram_id: Option<i64>,
    pub whatsapp_id: Option<String>,
    pub password: Option<String>,
}

pub async fn update_user(
    State(state): State<DashState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let role = body.role.as_deref().map(crate::users::UserRole::from_str);

    let user = state.agent.user_manager
        .update(&id, body.display_name.as_deref(), role, body.email.as_deref(), body.enabled)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    if let Some(tg_id) = body.telegram_id {
        let _ = state.agent.user_manager.link_telegram(&user.id, tg_id).await;
    }
    if let Some(ref wa_id) = body.whatsapp_id {
        let _ = state.agent.user_manager.link_whatsapp(&user.id, wa_id).await;
    }
    if let Some(ref pw) = body.password {
        let _ = state.agent.user_manager.set_password(&user.id, pw).await;
    }

    let user = state.agent.user_manager.get_by_id(&user.id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!(user)))
}

/// Delete a user.
pub async fn delete_user(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Json<ActionResponse> {
    match state.agent.user_manager.delete(&id).await {
        Ok(()) => Json(ActionResponse { ok: true, message: Some("User deleted".into()), count: None }),
        Err(e) => Json(ActionResponse { ok: false, message: Some(format!("Failed: {e}")), count: None }),
    }
}

// -- Onboarding Wizard -------------------------------------------------------

/// Returns the current onboarding status plus relevant config info.
pub async fn onboarding_status(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let completed = {
        let db = state.db.lock().await;
        db.query_row(
            "SELECT value FROM metadata WHERE key = 'onboarding_completed'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|v| v == "true")
        .unwrap_or(false)
    };

    let active_backend = state.agent.llm.active_backend();
    let available_backends = state.agent.llm.available_backends();
    let telegram_enabled = state.config.telegram.enabled;
    let whatsapp_enabled = state.config.whatsapp.enabled;

    Json(serde_json::json!({
        "completed": completed,
        "agent_name": state.config.agent_name,
        "core_personality": state.config.core_personality,
        "llm_backend": active_backend,
        "llm_available": available_backends,
        "telegram_enabled": telegram_enabled,
        "whatsapp_enabled": whatsapp_enabled,
    }))
}

/// Mark onboarding as complete, optionally saving agent_name / core_personality.
#[derive(Deserialize)]
pub struct OnboardingCompleteBody {
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub core_personality: Option<String>,
}

pub async fn onboarding_complete(
    State(state): State<DashState>,
    Json(body): Json<OnboardingCompleteBody>,
) -> Json<ActionResponse> {
    // Optionally persist agent_name / core_personality to config file.
    if body.agent_name.is_some() || body.core_personality.is_some() {
        if let Err(e) = write_partial_config(body.agent_name.as_deref(), body.core_personality.as_deref(), None) {
            error!("failed to save config during onboarding complete: {e}");
        }
    }

    let db = state.db.lock().await;
    let result = db.execute(
        "INSERT INTO metadata (key, value) VALUES ('onboarding_completed', 'true')
         ON CONFLICT(key) DO UPDATE SET value = 'true'",
        [],
    );

    match result {
        Ok(_) => {
            info!("onboarding marked complete");
            Json(ActionResponse { ok: true, message: Some("Onboarding complete".into()), count: None })
        }
        Err(e) => Json(ActionResponse { ok: false, message: Some(format!("DB error: {e}")), count: None }),
    }
}

/// Send a tiny prompt through the active LLM backend and return the result.
pub async fn onboarding_test_llm(
    State(state): State<DashState>,
) -> Json<serde_json::Value> {
    let gen_ctx = crate::llm::GenerateContext {
        message: "Say hello in one sentence.",
        tools: None,
        prompt_skills: &[],
        images: Vec::new(),
    };
    match state.agent.llm.generate(&gen_ctx).await {
        Ok(response) => Json(serde_json::json!({
            "ok": true,
            "response": response.trim(),
        })),
        Err(e) => Json(serde_json::json!({
            "ok": false,
            "error": format!("{e}"),
        })),
    }
}

/// Accept partial config fields and write them to the TOML config file on disk.
#[derive(Deserialize)]
pub struct SaveConfigBody {
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub core_personality: Option<String>,
    #[serde(default)]
    pub llm_backend: Option<String>,
}

pub async fn onboarding_save_config(
    State(_state): State<DashState>,
    Json(body): Json<SaveConfigBody>,
) -> Json<ActionResponse> {
    match write_partial_config(body.agent_name.as_deref(), body.core_personality.as_deref(), body.llm_backend.as_deref()) {
        Ok(()) => Json(ActionResponse { ok: true, message: Some("Config saved".into()), count: None }),
        Err(e) => Json(ActionResponse { ok: false, message: Some(format!("Failed: {e}")), count: None }),
    }
}

/// Helper: read the existing config TOML, patch the given fields, and write it
/// back.  Creates the file with defaults if it doesn't exist yet.
fn write_partial_config(
    agent_name: Option<&str>,
    core_personality: Option<&str>,
    llm_backend: Option<&str>,
) -> std::result::Result<(), String> {
    use crate::config::Config;
    let config_path = Config::default_config_path();

    // Ensure parent directory exists.
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }

    // Read existing file or start from the example template.
    let mut contents = if config_path.exists() {
        std::fs::read_to_string(&config_path).map_err(|e| format!("read: {e}"))?
    } else {
        Config::default_config_contents().to_string()
    };

    // Parse into a TOML table so we can patch individual keys safely.
    let mut doc: toml::Table = contents.parse::<toml::Table>().unwrap_or_default();

    if let Some(name) = agent_name {
        doc.insert("agent_name".into(), toml::Value::String(name.to_string()));
    }
    if let Some(personality) = core_personality {
        doc.insert("core_personality".into(), toml::Value::String(personality.to_string()));
    }
    if let Some(backend) = llm_backend {
        let llm = doc.entry("llm").or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = llm {
            t.insert("backend".into(), toml::Value::String(backend.to_string()));
        }
    }

    contents = toml::to_string_pretty(&doc).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&config_path, &contents).map_err(|e| format!("write: {e}"))?;

    info!("wrote updated config to {}", config_path.display());
    Ok(())
}

// -- Timezone & Locale -------------------------------------------------------

/// Get the system default timezone/locale and the current user's overrides.
pub async fn get_timezone(
    State(state): State<DashState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let system_tz = &state.config.timezone;
    let system_locale = &state.config.locale;

    // If a user_id is provided, return their per-user settings too.
    let (user_tz, user_locale) = if let Some(uid) = params.get("user_id") {
        match state.agent.user_manager.get_by_id(uid).await {
            Ok(u) => (u.timezone, u.locale),
            Err(_) => (String::new(), String::new()),
        }
    } else {
        (String::new(), String::new())
    };

    // Effective timezone: user override > system default
    let effective_tz = if user_tz.is_empty() { system_tz } else { &user_tz };
    let effective_locale = if user_locale.is_empty() { system_locale } else { &user_locale };

    // Compute current time in the effective timezone.
    let tz: chrono_tz::Tz = effective_tz.parse().unwrap_or(chrono_tz::UTC);
    let now_local = chrono::Utc::now().with_timezone(&tz);

    Json(serde_json::json!({
        "system_timezone": system_tz,
        "system_locale": system_locale,
        "user_timezone": user_tz,
        "user_locale": user_locale,
        "effective_timezone": effective_tz,
        "effective_locale": effective_locale,
        "current_time": now_local.to_rfc3339(),
        "current_time_formatted": now_local.format("%A, %B %-d, %Y at %-I:%M %p %Z").to_string(),
    }))
}

/// Set a user's timezone and/or locale.
#[derive(Deserialize)]
pub struct SetTimezoneBody {
    pub user_id: String,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
}

pub async fn set_timezone(
    State(state): State<DashState>,
    Json(body): Json<SetTimezoneBody>,
) -> Json<ActionResponse> {
    if let Some(ref tz) = body.timezone {
        // Validate the timezone name
        if tz.parse::<chrono_tz::Tz>().is_err() && !tz.is_empty() {
            return Json(ActionResponse {
                ok: false,
                message: Some(format!("Invalid IANA timezone: '{tz}'")),
                count: None,
            });
        }
        if let Err(e) = state.agent.user_manager.set_timezone(&body.user_id, tz).await {
            return Json(ActionResponse { ok: false, message: Some(format!("Failed: {e}")), count: None });
        }
    }
    if let Some(ref locale) = body.locale {
        if let Err(e) = state.agent.user_manager.set_locale(&body.user_id, locale).await {
            return Json(ActionResponse { ok: false, message: Some(format!("Failed: {e}")), count: None });
        }
    }
    Json(ActionResponse { ok: true, message: Some("Timezone/locale updated".into()), count: None })
}

/// List all available IANA timezone names, grouped by region.
pub async fn list_timezones() -> Json<serde_json::Value> {
    use chrono_tz::TZ_VARIANTS;
    let names: Vec<&str> = TZ_VARIANTS.iter().map(|tz| tz.name()).collect();
    Json(serde_json::json!({ "timezones": names }))
}

/// Convert a UTC timestamp to the given timezone and return formatted strings.
#[derive(Deserialize)]
pub struct ConvertTimeQuery {
    pub utc: String,
    #[serde(default = "default_utc_str")]
    pub timezone: String,
}

fn default_utc_str() -> String { "UTC".to_string() }

pub async fn convert_time(
    Query(params): Query<ConvertTimeQuery>,
) -> Json<serde_json::Value> {
    let tz: chrono_tz::Tz = params.timezone.parse().unwrap_or(chrono_tz::UTC);

    // Try parsing as RFC3339, or as SQLite datetime format
    let utc_dt = chrono::DateTime::parse_from_rfc3339(&params.utc)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(&params.utc, "%Y-%m-%d %H:%M:%S")
                .map(|ndt| ndt.and_utc())
        });

    match utc_dt {
        Ok(dt) => {
            let local = dt.with_timezone(&tz);
            Json(serde_json::json!({
                "ok": true,
                "utc": dt.to_rfc3339(),
                "local": local.to_rfc3339(),
                "formatted": local.format("%b %-d, %Y %-I:%M %p %Z").to_string(),
                "date": local.format("%Y-%m-%d").to_string(),
                "time": local.format("%-I:%M %p").to_string(),
                "relative_day": relative_day_label(&dt, &tz),
            }))
        }
        Err(_) => Json(serde_json::json!({
            "ok": false,
            "error": "Could not parse UTC timestamp",
        })),
    }
}

// -- LLM Advisor & Ollama Management ----------------------------------------

pub async fn llm_system_specs() -> Json<serde_json::Value> {
    let report = crate::llm::advisor::detect_system();
    Json(serde_json::json!(report))
}

#[derive(Deserialize)]
pub struct RecommendQuery {
    #[serde(default)]
    pub use_case: Option<String>,
    #[serde(default = "default_recommend_limit")]
    pub limit: usize,
}
fn default_recommend_limit() -> usize { 20 }

pub async fn llm_recommend(
    Query(params): Query<RecommendQuery>,
) -> Json<serde_json::Value> {
    let recommendations = crate::llm::advisor::recommend_models(
        params.use_case.as_deref(),
        params.limit,
    );
    Json(serde_json::json!({ "models": recommendations }))
}

pub async fn ollama_status() -> Json<serde_json::Value> {
    let status = crate::llm::advisor::check_ollama();
    Json(serde_json::json!(status))
}

#[derive(Deserialize)]
pub struct OllamaPullRequest {
    pub tag: String,
}

pub async fn ollama_pull(
    Json(body): Json<OllamaPullRequest>,
) -> impl IntoResponse {
    use llmfit_core::providers::{ModelProvider, OllamaProvider, PullEvent};

    let provider = OllamaProvider::default();
    if !provider.is_available() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "ok": false, "error": "Ollama is not running" })),
        );
    }

    match provider.start_pull(&body.tag) {
        Ok(handle) => {
            let mut last_status = String::new();
            let mut last_percent: Option<f64> = None;
            loop {
                match handle.receiver.recv() {
                    Ok(PullEvent::Progress { status, percent }) => {
                        last_status = status;
                        last_percent = percent;
                    }
                    Ok(PullEvent::Done) => {
                        return (
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "ok": true,
                                "tag": body.tag,
                                "status": "complete",
                            })),
                        );
                    }
                    Ok(PullEvent::Error(msg)) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({
                                "ok": false,
                                "error": msg,
                            })),
                        );
                    }
                    Err(_) => {
                        if last_status.is_empty() {
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Json(serde_json::json!({
                                    "ok": false,
                                    "error": "Pull channel closed unexpectedly",
                                })),
                            );
                        }
                        return (
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "ok": true,
                                "tag": body.tag,
                                "status": last_status,
                                "percent": last_percent,
                            })),
                        );
                    }
                }
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "error": e })),
        ),
    }
}

pub async fn ollama_delete(
    Path(tag): Path<String>,
) -> impl IntoResponse {
    let host = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());

    let client = reqwest::Client::new();
    let resp = client
        .delete(format!("{}/api/delete", host))
        .json(&serde_json::json!({ "name": tag }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "deleted": tag })),
        ),
        Ok(r) => {
            let msg = r.text().await.unwrap_or_default();
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "ok": false, "error": msg })),
            )
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
        ),
    }
}

#[derive(Deserialize)]
pub struct OllamaConfigureRequest {
    pub model: String,
}

pub async fn ollama_configure(
    State(_state): State<DashState>,
    Json(body): Json<OllamaConfigureRequest>,
) -> Json<serde_json::Value> {
    info!(model = %body.model, "setting Ollama as active backend");
    unsafe { std::env::set_var("OLLAMA_MODEL", &body.model); }
    unsafe { std::env::set_var("LLM_BACKEND", "ollama"); }

    Json(serde_json::json!({
        "ok": true,
        "backend": "ollama",
        "model": body.model,
        "note": "Backend switch takes effect on next LLM request. For persistence, update config.toml.",
    }))
}

/// Return a human-friendly label like "Today", "Yesterday", or the date.
fn relative_day_label(utc_dt: &chrono::DateTime<chrono::Utc>, tz: &chrono_tz::Tz) -> String {
    let local = utc_dt.with_timezone(tz);
    let today = chrono::Utc::now().with_timezone(tz).date_naive();
    let dt_date = local.date_naive();

    if dt_date == today {
        "Today".to_string()
    } else if dt_date == today.pred_opt().unwrap_or(today) {
        "Yesterday".to_string()
    } else {
        local.format("%b %-d, %Y").to_string()
    }
}
