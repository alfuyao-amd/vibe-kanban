use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, TS)]
pub struct ProjectLeadAgent {
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    #[ts(type = "Date")]
    pub created_at: DateTime<Utc>,
    #[ts(type = "Date")]
    pub updated_at: DateTime<Utc>,
}

impl ProjectLeadAgent {
    const COLUMNS: &'static str = "project_id, session_id, workspace_id, created_at, updated_at";

    pub async fn find_for_project(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<Option<Self>, sqlx::Error> {
        let query = format!(
            r#"SELECT {cols} FROM project_lead_agents WHERE project_id = ?"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(project_id)
            .fetch_optional(pool)
            .await
    }

    /// Look up the lead-agent record for a given session. Returns `None` for
    /// sessions that are not project-lead-agent sessions.
    pub async fn find_by_session_id(
        pool: &SqlitePool,
        session_id: Uuid,
    ) -> Result<Option<Self>, sqlx::Error> {
        let query = format!(
            r#"SELECT {cols} FROM project_lead_agents WHERE session_id = ?"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(session_id)
            .fetch_optional(pool)
            .await
    }

    /// All lead-agent records whose bound workspace matches `workspace_id`.
    /// In practice this is at most one (a workspace can host the lead agent
    /// for only one project today), but the query is shaped as a list so
    /// callers don't have to assume cardinality.
    pub async fn list_for_workspace(
        pool: &SqlitePool,
        workspace_id: Uuid,
    ) -> Result<Vec<Self>, sqlx::Error> {
        let query = format!(
            r#"SELECT {cols} FROM project_lead_agents WHERE workspace_id = ?"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(workspace_id)
            .fetch_all(pool)
            .await
    }

    pub async fn upsert(
        pool: &SqlitePool,
        project_id: Uuid,
        session_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Self, sqlx::Error> {
        let query = format!(
            r#"INSERT INTO project_lead_agents (project_id, session_id, workspace_id)
               VALUES (?, ?, ?)
               ON CONFLICT (project_id) DO UPDATE SET
                   session_id = excluded.session_id,
                   workspace_id = excluded.workspace_id,
                   updated_at = datetime('now', 'subsec')
               RETURNING {cols}"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(project_id)
            .bind(session_id)
            .bind(workspace_id)
            .fetch_one(pool)
            .await
    }
}
