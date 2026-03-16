use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use tracing::info;

use crate::error::Result;

pub fn open(path: &Path) -> Result<Connection> {
    info!("opening database at {}", path.display());
    let conn = Connection::open(path)?;

    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    migrate(&conn)?;
    Ok(conn)
}

/// Open a read-only database connection for SELECT queries.
/// Uses SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX to reduce mutex contention.
/// Must be called after the main connection has been opened and migrated.
pub fn open_readonly(path: &Path) -> Result<Connection> {
    info!("opening read-only database connection at {}", path.display());
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    conn.execute_batch("PRAGMA query_only = ON;")?;

    Ok(conn)
}

/// Run database migrations. Exposed for tests that use in-memory DBs.
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        -- Conversation history
        CREATE TABLE IF NOT EXISTS conversation_history (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            role        TEXT NOT NULL,
            content     TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Core memory (single-row personality)
        CREATE TABLE IF NOT EXISTS core_memory (
            id          INTEGER PRIMARY KEY CHECK (id = 1),
            personality TEXT NOT NULL,
            updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Archival memory
        CREATE TABLE IF NOT EXISTS archival_memory (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            content     TEXT NOT NULL,
            category    TEXT NOT NULL DEFAULT '',
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS archival_memory_fts USING fts5(
            content,
            category,
            content='archival_memory',
            content_rowid='id'
        );

        CREATE TRIGGER IF NOT EXISTS archival_ai AFTER INSERT ON archival_memory BEGIN
            INSERT INTO archival_memory_fts(rowid, content, category)
            VALUES (new.id, new.content, new.category);
        END;

        CREATE TRIGGER IF NOT EXISTS archival_ad AFTER DELETE ON archival_memory BEGIN
            INSERT INTO archival_memory_fts(archival_memory_fts, rowid, content, category)
            VALUES ('delete', old.id, old.content, old.category);
        END;

        CREATE TRIGGER IF NOT EXISTS archival_au AFTER UPDATE ON archival_memory BEGIN
            INSERT INTO archival_memory_fts(archival_memory_fts, rowid, content, category)
            VALUES ('delete', old.id, old.content, old.category);
            INSERT INTO archival_memory_fts(rowid, content, category)
            VALUES (new.id, new.content, new.category);
        END;

        -- Activity log
        CREATE TABLE IF NOT EXISTS activity_log (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            action_type TEXT NOT NULL,
            summary     TEXT NOT NULL,
            detail      TEXT,
            status      TEXT NOT NULL DEFAULT 'ok',
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Pending actions (approval queue)
        CREATE TABLE IF NOT EXISTS pending_actions (
            id          TEXT PRIMARY KEY,
            action_json TEXT NOT NULL,
            reasoning   TEXT NOT NULL DEFAULT '',
            context     TEXT NOT NULL DEFAULT '',
            status      TEXT NOT NULL DEFAULT 'pending',
            proposed_at TEXT NOT NULL DEFAULT (datetime('now')),
            resolved_at TEXT
        );

        -- Agent stats
        CREATE TABLE IF NOT EXISTS agent_stats (
            id              INTEGER PRIMARY KEY CHECK (id = 1),
            total_ticks     INTEGER NOT NULL DEFAULT 0,
            total_actions   INTEGER NOT NULL DEFAULT 0,
            total_approved  INTEGER NOT NULL DEFAULT 0,
            total_rejected  INTEGER NOT NULL DEFAULT 0,
            last_tick_at    TEXT,
            started_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT OR IGNORE INTO agent_stats (id) VALUES (1);

        -- Knowledge graph: nodes
        CREATE TABLE IF NOT EXISTS knowledge_nodes (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            label       TEXT NOT NULL,
            node_type   TEXT NOT NULL DEFAULT '',
            content     TEXT NOT NULL DEFAULT '',
            confidence  REAL NOT NULL DEFAULT 1.0,
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Knowledge graph: edges
        CREATE TABLE IF NOT EXISTS knowledge_edges (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id   INTEGER NOT NULL REFERENCES knowledge_nodes(id) ON DELETE CASCADE,
            target_id   INTEGER NOT NULL REFERENCES knowledge_nodes(id) ON DELETE CASCADE,
            relation    TEXT NOT NULL,
            weight      REAL NOT NULL DEFAULT 1.0,
            metadata    TEXT NOT NULL DEFAULT '{}',
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(source_id, target_id, relation)
        );

        -- Knowledge graph: FTS index
        CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_nodes_fts USING fts5(
            label, content, node_type,
            content='knowledge_nodes',
            content_rowid='id'
        );

        CREATE TRIGGER IF NOT EXISTS knowledge_ai AFTER INSERT ON knowledge_nodes BEGIN
            INSERT INTO knowledge_nodes_fts(rowid, label, content, node_type)
            VALUES (new.id, new.label, new.content, new.node_type);
        END;

        CREATE TRIGGER IF NOT EXISTS knowledge_ad AFTER DELETE ON knowledge_nodes BEGIN
            INSERT INTO knowledge_nodes_fts(knowledge_nodes_fts, rowid, label, content, node_type)
            VALUES ('delete', old.id, old.label, old.content, old.node_type);
        END;

        CREATE TRIGGER IF NOT EXISTS knowledge_au AFTER UPDATE ON knowledge_nodes BEGIN
            INSERT INTO knowledge_nodes_fts(knowledge_nodes_fts, rowid, label, content, node_type)
            VALUES ('delete', old.id, old.label, old.content, old.node_type);
            INSERT INTO knowledge_nodes_fts(rowid, label, content, node_type)
            VALUES (new.id, new.label, new.content, new.node_type);
        END;

        -- OAuth tokens (multi-account per provider)
        CREATE TABLE IF NOT EXISTS oauth_tokens (
            provider      TEXT NOT NULL,
            account       TEXT NOT NULL DEFAULT '',
            email         TEXT NOT NULL DEFAULT '',
            access_token  TEXT NOT NULL,
            refresh_token TEXT,
            expires_at    TEXT,
            scopes        TEXT NOT NULL DEFAULT '',
            created_at    TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (provider, account)
        );

        -- Cron jobs
        CREATE TABLE IF NOT EXISTS cron_jobs (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            schedule    TEXT NOT NULL,
            tool_call   TEXT NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            last_run_at TEXT,
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Sessions (multi-agent)
        CREATE TABLE IF NOT EXISTS sessions (
            id          TEXT PRIMARY KEY,
            label       TEXT NOT NULL DEFAULT '',
            agent_id    TEXT NOT NULL DEFAULT 'default',
            status      TEXT NOT NULL DEFAULT 'active',
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS session_messages (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            role        TEXT NOT NULL,
            content     TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Goals (background objectives the agent works on autonomously)
        CREATE TABLE IF NOT EXISTS goals (
            id             TEXT PRIMARY KEY,
            title          TEXT NOT NULL,
            description    TEXT NOT NULL DEFAULT '',
            status         TEXT NOT NULL DEFAULT 'active',   -- active, paused, completed, failed, cancelled
            priority       INTEGER NOT NULL DEFAULT 0,        -- higher = more important
            parent_goal_id TEXT REFERENCES goals(id) ON DELETE SET NULL,
            reflection     TEXT,                              -- self-reflection after completion
            created_at     TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at     TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at   TEXT
        );

        -- Goal tasks (subtasks within a goal)
        CREATE TABLE IF NOT EXISTS goal_tasks (
            id           TEXT PRIMARY KEY,
            goal_id      TEXT NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
            title        TEXT NOT NULL,
            description  TEXT NOT NULL DEFAULT '',
            status       TEXT NOT NULL DEFAULT 'pending',     -- pending, in_progress, completed, failed, skipped
            tool_call    TEXT,                                 -- JSON: { tool, params, reasoning }
            depends_on   TEXT,                                 -- comma-separated task IDs
            result       TEXT,                                 -- output from execution
            sort_order   INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at TEXT
        );

        -- Audit trail (structured log of every tool call, approval decision, LLM call)
        CREATE TABLE IF NOT EXISTS audit_log (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            event_type   TEXT NOT NULL,          -- tool_call, approval, llm_call, rate_limit, pii_detected, 2fa
            tool         TEXT,                    -- tool name (if applicable)
            action       TEXT,                    -- approve/reject/execute/block/redact
            user_context TEXT,                    -- what triggered this (user message, cron, goal, etc.)
            reasoning    TEXT,                    -- LLM reasoning for the action
            params_json  TEXT,                    -- tool params (JSON)
            result       TEXT,                    -- output or result summary
            success      INTEGER,                 -- 1 = success, 0 = failure
            source       TEXT NOT NULL DEFAULT 'agent',  -- agent, dashboard, telegram, whatsapp, cron, goal
            created_at   TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_audit_log_type ON audit_log(event_type);
        CREATE INDEX IF NOT EXISTS idx_audit_log_tool ON audit_log(tool);
        CREATE INDEX IF NOT EXISTS idx_audit_log_created ON audit_log(created_at);

        -- LLM usage tracking (token counts and estimated costs)
        CREATE TABLE IF NOT EXISTS llm_usage (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            backend           TEXT NOT NULL,       -- claude, openrouter, gemini, etc.
            model             TEXT NOT NULL DEFAULT '',
            prompt_tokens     INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens      INTEGER NOT NULL DEFAULT 0,
            estimated_cost    REAL NOT NULL DEFAULT 0.0,  -- USD
            context           TEXT NOT NULL DEFAULT '',     -- message, goal_task, cron, follow_up
            created_at        TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_llm_usage_created ON llm_usage(created_at);
        CREATE INDEX IF NOT EXISTS idx_llm_usage_backend ON llm_usage(backend);

        -- Users (multi-user support)
        CREATE TABLE IF NOT EXISTS users (
            id              TEXT PRIMARY KEY,                   -- UUID
            username        TEXT NOT NULL UNIQUE,               -- login name
            display_name    TEXT NOT NULL DEFAULT '',
            role            TEXT NOT NULL DEFAULT 'user',       -- admin, user, viewer
            email           TEXT NOT NULL DEFAULT '',
            password_hash   TEXT NOT NULL DEFAULT '',           -- plaintext for now; bcrypt later
            telegram_id     INTEGER,                            -- Telegram user ID mapping
            whatsapp_id     TEXT,                               -- WhatsApp JID/number mapping
            enabled         INTEGER NOT NULL DEFAULT 1,
            last_seen_at    TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_users_telegram ON users(telegram_id) WHERE telegram_id IS NOT NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_users_whatsapp ON users(whatsapp_id) WHERE whatsapp_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_users_email ON users(email) WHERE email != '';
        ",
    )?;

    // --- Add user_id columns to existing tables if missing ---
    add_column_if_missing(conn, "conversation_history", "user_id", "TEXT DEFAULT NULL");
    add_column_if_missing(conn, "activity_log", "user_id", "TEXT DEFAULT NULL");
    add_column_if_missing(conn, "audit_log", "user_id", "TEXT DEFAULT NULL");
    add_column_if_missing(conn, "goals", "user_id", "TEXT DEFAULT NULL");
    add_column_if_missing(conn, "pending_actions", "user_id", "TEXT DEFAULT NULL");

    // --- Add 2FA columns to users table if missing ---
    add_column_if_missing(conn, "users", "totp_secret", "TEXT DEFAULT NULL");
    add_column_if_missing(conn, "users", "totp_enabled", "INTEGER NOT NULL DEFAULT 0");
    add_column_if_missing(conn, "users", "recovery_codes", "TEXT DEFAULT NULL");

    // --- Timezone & locale per-user columns ---
    add_column_if_missing(conn, "users", "timezone", "TEXT NOT NULL DEFAULT ''");
    add_column_if_missing(conn, "users", "locale", "TEXT NOT NULL DEFAULT ''");

    // --- PII encryption: blind index columns for encrypted lookup fields ---
    add_column_if_missing(conn, "users", "email_blind", "TEXT NOT NULL DEFAULT ''");
    add_column_if_missing(conn, "users", "telegram_id_blind", "TEXT NOT NULL DEFAULT ''");
    add_column_if_missing(conn, "users", "whatsapp_id_blind", "TEXT NOT NULL DEFAULT ''");
    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_users_email_blind ON users(email_blind) WHERE email_blind != '';
        CREATE INDEX IF NOT EXISTS idx_users_telegram_blind ON users(telegram_id_blind) WHERE telegram_id_blind != '';
        CREATE INDEX IF NOT EXISTS idx_users_whatsapp_blind ON users(whatsapp_id_blind) WHERE whatsapp_id_blind != '';
        ",
    )?;

    // --- SMS/iMessage identity columns ---
    add_column_if_missing(conn, "users", "imessage_id", "TEXT");
    add_column_if_missing(conn, "users", "imessage_id_blind", "TEXT NOT NULL DEFAULT ''");
    add_column_if_missing(conn, "users", "twilio_number", "TEXT");
    add_column_if_missing(conn, "users", "twilio_number_blind", "TEXT NOT NULL DEFAULT ''");
    add_column_if_missing(conn, "users", "android_sms_id", "TEXT");
    add_column_if_missing(conn, "users", "android_sms_id_blind", "TEXT NOT NULL DEFAULT ''");
    add_column_if_missing(conn, "users", "discord_id", "TEXT");
    add_column_if_missing(conn, "users", "discord_id_blind", "TEXT NOT NULL DEFAULT ''");
    add_column_if_missing(conn, "users", "signal_id", "TEXT");
    add_column_if_missing(conn, "users", "signal_id_blind", "TEXT NOT NULL DEFAULT ''");
    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_users_imessage_blind ON users(imessage_id_blind) WHERE imessage_id_blind != '';
        CREATE INDEX IF NOT EXISTS idx_users_twilio_blind ON users(twilio_number_blind) WHERE twilio_number_blind != '';
        CREATE INDEX IF NOT EXISTS idx_users_android_sms_blind ON users(android_sms_id_blind) WHERE android_sms_id_blind != '';
        CREATE INDEX IF NOT EXISTS idx_users_discord_blind ON users(discord_id_blind) WHERE discord_id_blind != '';
        CREATE INDEX IF NOT EXISTS idx_users_signal_blind ON users(signal_id_blind) WHERE signal_id_blind != '';
        ",
    )?;

    // --- Passkeys table (WebAuthn credentials) ---
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS passkeys (
            id              TEXT PRIMARY KEY,          -- credential ID (base64url)
            user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            name            TEXT NOT NULL DEFAULT '',   -- friendly name
            credential_json TEXT NOT NULL,              -- serialized Passkey
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_passkeys_user ON passkeys(user_id);
        ",
    )?;

    // Create indexes on user_id columns
    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_conversation_user ON conversation_history(user_id) WHERE user_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_activity_user ON activity_log(user_id) WHERE user_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_audit_user ON audit_log(user_id) WHERE user_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_goals_user ON goals(user_id) WHERE user_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_pending_user ON pending_actions(user_id) WHERE user_id IS NOT NULL;
        ",
    )?;

    // -        -- Metadata key-value store (onboarding state, app-level flags) ---
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS metadata (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL DEFAULT ''
        );
        ",
    )?;

    // --- Episodic memory (what happened during each interaction) ---
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS episodes (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            trigger     TEXT NOT NULL,
            summary     TEXT NOT NULL DEFAULT '',
            actions     TEXT NOT NULL DEFAULT '[]',
            outcome     TEXT NOT NULL DEFAULT '',
            user_id     TEXT,
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_episodes_created ON episodes(created_at);
        CREATE INDEX IF NOT EXISTS idx_episodes_user ON episodes(user_id) WHERE user_id IS NOT NULL;
        ",
    )?;

    // --- User profiles (structured key-value user preferences) ---
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS user_profiles (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id     TEXT,
            key         TEXT NOT NULL,
            value       TEXT NOT NULL,
            confidence  REAL NOT NULL DEFAULT 1.0,
            source      TEXT NOT NULL DEFAULT '',
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(user_id, key)
        );

        CREATE INDEX IF NOT EXISTS idx_user_profiles_user ON user_profiles(user_id);
        ",
    )?;

    // --- Memory embeddings (vector representations for semantic search) ---
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS memory_embeddings (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            source_table TEXT NOT NULL,
            source_id    INTEGER NOT NULL,
            embedding    BLOB NOT NULL,
            model        TEXT NOT NULL DEFAULT '',
            created_at   TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(source_table, source_id)
        );

        CREATE INDEX IF NOT EXISTS idx_embeddings_source ON memory_embeddings(source_table, source_id);
        ",
    )?;

    // --- consolidated flag on archival_memory for decay tracking ---
    add_column_if_missing(conn, "archival_memory", "consolidated", "INTEGER NOT NULL DEFAULT 0");

    // Migrate oauth_tokens from single-account to multi-account schema.
    // Check if the 'account' column exists; if not, recreate the table.
    let has_account_col: bool = conn
        .prepare("SELECT account FROM oauth_tokens LIMIT 0")
        .is_ok();

    if !has_account_col {
        info!("migrating oauth_tokens to multi-account schema");
        conn.execute_batch(
            "
            ALTER TABLE oauth_tokens RENAME TO oauth_tokens_old;

            CREATE TABLE oauth_tokens (
                provider      TEXT NOT NULL,
                account       TEXT NOT NULL DEFAULT '',
                email         TEXT NOT NULL DEFAULT '',
                access_token  TEXT NOT NULL,
                refresh_token TEXT,
                expires_at    TEXT,
                scopes        TEXT NOT NULL DEFAULT '',
                created_at    TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (provider, account)
            );

            INSERT INTO oauth_tokens (provider, account, email, access_token, refresh_token, expires_at, scopes, created_at, updated_at)
                SELECT provider, 'default', '', access_token, refresh_token, expires_at, scopes, created_at, updated_at
                FROM oauth_tokens_old;

            DROP TABLE oauth_tokens_old;
            ",
        )?;
        info!("oauth_tokens migration complete");
    }

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS api_tokens (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            token_hash  TEXT NOT NULL UNIQUE,
            scopes      TEXT NOT NULL DEFAULT '*',
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            last_used   TEXT,
            enabled     INTEGER NOT NULL DEFAULT 1
        );
        ",
    )?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS personas (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            personality TEXT NOT NULL,
            tools       TEXT NOT NULL DEFAULT '',
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );
        ",
    )?;

    info!("database migrations complete");
    Ok(())
}

/// Add a column to a table if it doesn't already exist.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, col_type: &str) {
    let has_col = conn
        .prepare(&format!("SELECT {column} FROM {table} LIMIT 0"))
        .is_ok();
    if !has_col {
        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {col_type}");
        if let Err(e) = conn.execute_batch(&sql) {
            tracing::warn!(table, column, err = %e, "failed to add column (may already exist)");
        } else {
            info!(table, column, "added column via migration");
        }
    }
}

/// Creates an in-memory database with migrations applied. Use in tests.
#[cfg(test)]
pub(crate) fn test_db() -> std::sync::Arc<tokio::sync::Mutex<Connection>> {
    use std::sync::Arc;

    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    migrate(&conn).unwrap();
    Arc::new(tokio::sync::Mutex::new(conn))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_open_with_temp_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("safeclaw-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let conn = open(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        drop(conn);
    }

    #[test]
    fn test_all_tables_exist_after_migration() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&conn).unwrap();

        let tables = [
            "conversation_history",
            "core_memory",
            "archival_memory",
            "archival_memory_fts",
            "activity_log",
            "pending_actions",
            "agent_stats",
            "knowledge_nodes",
            "knowledge_edges",
            "knowledge_nodes_fts",
            "oauth_tokens",
            "cron_jobs",
            "sessions",
            "session_messages",
            "goals",
            "goal_tasks",
            "audit_log",
            "llm_usage",
            "users",
            "passkeys",
            "metadata",
            "episodes",
            "user_profiles",
            "memory_embeddings",
            "api_tokens",
            "personas",
        ];

        for table in tables {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "table {} should exist", table);
        }
    }

    #[test]
    fn test_migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
    }
}
