use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ProcedureSource {
    User,
    LeadAgent,
}

impl ProcedureSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::LeadAgent => "lead_agent",
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, TS)]
pub struct ProcedureRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub version: i64,
    pub yaml: String,
    pub source: String,
    #[ts(type = "Date")]
    pub created_at: DateTime<Utc>,
    #[ts(type = "Date")]
    pub updated_at: DateTime<Utc>,
}

impl ProcedureRecord {
    const COLUMNS: &'static str =
        "id, project_id, name, version, yaml, source, created_at, updated_at";

    pub async fn list_for_project(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<Vec<Self>, sqlx::Error> {
        let query = format!(
            r#"SELECT {cols}
               FROM procedures
               WHERE project_id = ?
               ORDER BY name ASC"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(project_id)
            .fetch_all(pool)
            .await
    }

    pub async fn find_by_name(
        pool: &SqlitePool,
        project_id: Uuid,
        name: &str,
    ) -> Result<Option<Self>, sqlx::Error> {
        let query = format!(
            r#"SELECT {cols}
               FROM procedures
               WHERE project_id = ? AND name = ?"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(project_id)
            .bind(name)
            .fetch_optional(pool)
            .await
    }

    pub async fn upsert(
        pool: &SqlitePool,
        project_id: Uuid,
        name: &str,
        yaml: &str,
        source: ProcedureSource,
    ) -> Result<Self, sqlx::Error> {
        // Bump version on update; start at 1 on insert.
        let query = format!(
            r#"INSERT INTO procedures (id, project_id, name, version, yaml, source)
               VALUES (?, ?, ?, 1, ?, ?)
               ON CONFLICT (project_id, name) DO UPDATE SET
                   yaml = excluded.yaml,
                   source = excluded.source,
                   version = procedures.version + 1,
                   updated_at = datetime('now', 'subsec')
               RETURNING {cols}"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(Uuid::new_v4())
            .bind(project_id)
            .bind(name)
            .bind(yaml)
            .bind(source.as_str())
            .fetch_one(pool)
            .await
    }

    pub async fn delete(
        pool: &SqlitePool,
        project_id: Uuid,
        name: &str,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM procedures WHERE project_id = ? AND name = ?")
            .bind(project_id)
            .bind(name)
            .execute(pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
