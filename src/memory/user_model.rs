use std::sync::Arc;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub id: i64,
    pub user_id: Option<String>,
    pub key: String,
    pub value: String,
    pub confidence: f64,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

pub struct UserModel {
    db: Arc<Mutex<Connection>>,
}

impl UserModel {
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self { db }
    }

    /// Set or update a user profile entry.  Uses UPSERT to merge.
    pub async fn set(
        &self,
        user_id: Option<&str>,
        key: &str,
        value: &str,
        confidence: f64,
        source: &str,
    ) -> Result<()> {
        let db = self.db.lock().await;
        let uid = user_id.unwrap_or("");
        db.execute(
            "INSERT INTO user_profiles (user_id, key, value, confidence, source)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id, key) DO UPDATE SET
                 value = excluded.value,
                 confidence = excluded.confidence,
                 source = excluded.source,
                 updated_at = datetime('now')",
            rusqlite::params![uid, key, value, confidence, source],
        )?;
        Ok(())
    }

    /// Get a specific profile entry by key.
    pub async fn get(&self, user_id: Option<&str>, key: &str) -> Result<Option<ProfileEntry>> {
        let db = self.db.lock().await;
        let uid = user_id.unwrap_or("");
        let result = db.query_row(
            "SELECT id, user_id, key, value, confidence, source, created_at, updated_at
             FROM user_profiles WHERE user_id = ?1 AND key = ?2",
            rusqlite::params![uid, key],
            map_profile,
        );

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the full profile for a user (all key-value pairs).
    pub async fn get_all(&self, user_id: Option<&str>) -> Result<Vec<ProfileEntry>> {
        let db = self.db.lock().await;
        let uid = user_id.unwrap_or("");
        let mut stmt = db.prepare(
            "SELECT id, user_id, key, value, confidence, source, created_at, updated_at
             FROM user_profiles WHERE user_id = ?1 ORDER BY key",
        )?;
        let entries = stmt
            .query_map([uid], map_profile)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    /// Remove a profile entry.
    pub async fn remove(&self, user_id: Option<&str>, key: &str) -> Result<bool> {
        let db = self.db.lock().await;
        let uid = user_id.unwrap_or("");
        let changed = db.execute(
            "DELETE FROM user_profiles WHERE user_id = ?1 AND key = ?2",
            rusqlite::params![uid, key],
        )?;
        Ok(changed > 0)
    }

    /// Format the user profile as a readable string for inclusion in LLM context.
    pub async fn as_context_string(&self, user_id: Option<&str>) -> Result<String> {
        let entries = self.get_all(user_id).await?;
        if entries.is_empty() {
            return Ok(String::new());
        }

        let mut ctx = String::from("== USER PROFILE ==\n");
        for entry in &entries {
            ctx.push_str(&format!("- {}: {}\n", entry.key, entry.value));
        }
        Ok(ctx)
    }
}

fn map_profile(row: &rusqlite::Row) -> rusqlite::Result<ProfileEntry> {
    Ok(ProfileEntry {
        id: row.get(0)?,
        user_id: row.get(1)?,
        key: row.get(2)?,
        value: row.get(3)?,
        confidence: row.get(4)?,
        source: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    #[tokio::test]
    async fn set_and_get() {
        let db = test_db();
        let um = UserModel::new(db);

        um.set(None, "preferred_language", "Rust", 0.9, "conversation")
            .await
            .unwrap();

        let entry = um.get(None, "preferred_language").await.unwrap().unwrap();
        assert_eq!(entry.value, "Rust");
        assert!((entry.confidence - 0.9).abs() < 0.001);
    }

    #[tokio::test]
    async fn upsert_overwrites() {
        let db = test_db();
        let um = UserModel::new(db);

        um.set(None, "topic", "AI", 0.5, "conv1").await.unwrap();
        um.set(None, "topic", "Robotics", 0.8, "conv2")
            .await
            .unwrap();

        let entry = um.get(None, "topic").await.unwrap().unwrap();
        assert_eq!(entry.value, "Robotics");
        assert!((entry.confidence - 0.8).abs() < 0.001);
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let db = test_db();
        let um = UserModel::new(db);
        assert!(um.get(None, "nonexistent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn get_all_profiles() {
        let db = test_db();
        let um = UserModel::new(db);

        um.set(None, "a_key", "val1", 1.0, "test").await.unwrap();
        um.set(None, "b_key", "val2", 1.0, "test").await.unwrap();

        let all = um.get_all(None).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].key, "a_key");
        assert_eq!(all[1].key, "b_key");
    }

    #[tokio::test]
    async fn remove_entry() {
        let db = test_db();
        let um = UserModel::new(db);

        um.set(None, "temp", "data", 1.0, "test").await.unwrap();
        assert!(um.remove(None, "temp").await.unwrap());
        assert!(um.get(None, "temp").await.unwrap().is_none());
        assert!(!um.remove(None, "temp").await.unwrap());
    }

    #[tokio::test]
    async fn user_scoped_profiles() {
        let db = test_db();
        let um = UserModel::new(db);

        um.set(Some("u1"), "lang", "Python", 1.0, "test")
            .await
            .unwrap();
        um.set(Some("u2"), "lang", "Go", 1.0, "test")
            .await
            .unwrap();

        let u1 = um.get(Some("u1"), "lang").await.unwrap().unwrap();
        assert_eq!(u1.value, "Python");

        let u2 = um.get(Some("u2"), "lang").await.unwrap().unwrap();
        assert_eq!(u2.value, "Go");
    }

    #[tokio::test]
    async fn context_string_format() {
        let db = test_db();
        let um = UserModel::new(db);

        um.set(None, "interests", "AI, Rust", 1.0, "test")
            .await
            .unwrap();
        um.set(None, "timezone_pref", "evenings", 0.7, "test")
            .await
            .unwrap();

        let ctx = um.as_context_string(None).await.unwrap();
        assert!(ctx.contains("== USER PROFILE =="));
        assert!(ctx.contains("interests: AI, Rust"));
        assert!(ctx.contains("timezone_pref: evenings"));
    }

    #[tokio::test]
    async fn context_string_empty() {
        let db = test_db();
        let um = UserModel::new(db);
        let ctx = um.as_context_string(None).await.unwrap();
        assert!(ctx.is_empty());
    }
}
