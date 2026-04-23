use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ProcedureRunStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl ProcedureRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct StateHistoryEntry {
    pub state: String,
    #[ts(type = "Date")]
    pub entered_at: DateTime<Utc>,
    #[ts(type = "Date | null")]
    pub exited_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub outcome: Option<StateOutcome>,
    #[serde(default)]
    pub gate_summary: Option<String>,
    #[serde(default)]
    pub attempt: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum StateOutcome {
    Success,
    Failure,
    Cancelled,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, TS)]
pub struct ProcedureRun {
    pub id: Uuid,
    pub project_id: Uuid,
    pub procedure_name: String,
    pub procedure_version: i64,
    pub current_state: String,
    pub status: String,
    #[ts(type = "Record<string, unknown>")]
    pub params: sqlx::types::Json<serde_json::Value>,
    #[ts(type = "Array<StateHistoryEntry>")]
    pub state_history: sqlx::types::Json<Vec<StateHistoryEntry>>,
    pub workspace_id: Option<Uuid>,
    #[ts(type = "Date")]
    pub created_at: DateTime<Utc>,
    #[ts(type = "Date")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, TS)]
pub struct CreateProcedureRun {
    pub procedure_name: String,
    pub procedure_version: i64,
    pub initial_state: String,
    #[ts(type = "Record<string, unknown>")]
    pub params: serde_json::Value,
    pub workspace_id: Option<Uuid>,
}

impl ProcedureRun {
    const COLUMNS: &'static str = "id, project_id, procedure_name, procedure_version, \
                                   current_state, status, params, state_history, \
                                   workspace_id, created_at, updated_at";

    pub async fn create(
        pool: &SqlitePool,
        project_id: Uuid,
        data: &CreateProcedureRun,
    ) -> Result<Self, sqlx::Error> {
        let id = Uuid::new_v4();
        let params_json = sqlx::types::Json(data.params.clone());
        let history: sqlx::types::Json<Vec<StateHistoryEntry>> = sqlx::types::Json(vec![]);
        let status = ProcedureRunStatus::Running.as_str();

        let query = format!(
            r#"INSERT INTO procedure_runs
               (id, project_id, procedure_name, procedure_version, current_state,
                status, params, state_history, workspace_id)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
               RETURNING {cols}"#,
            cols = Self::COLUMNS
        );

        sqlx::query_as::<_, Self>(&query)
            .bind(id)
            .bind(project_id)
            .bind(&data.procedure_name)
            .bind(data.procedure_version)
            .bind(&data.initial_state)
            .bind(status)
            .bind(params_json)
            .bind(history)
            .bind(data.workspace_id)
            .fetch_one(pool)
            .await
    }

    pub async fn find_by_id(pool: &SqlitePool, id: Uuid) -> Result<Option<Self>, sqlx::Error> {
        let query = format!(
            r#"SELECT {cols} FROM procedure_runs WHERE id = ?"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(id)
            .fetch_optional(pool)
            .await
    }

    pub async fn list_for_project(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<Vec<Self>, sqlx::Error> {
        let query = format!(
            r#"SELECT {cols}
               FROM procedure_runs
               WHERE project_id = ?
               ORDER BY created_at DESC"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(project_id)
            .fetch_all(pool)
            .await
    }

    pub async fn update_status(
        pool: &SqlitePool,
        id: Uuid,
        status: ProcedureRunStatus,
    ) -> Result<Option<Self>, sqlx::Error> {
        let query = format!(
            r#"UPDATE procedure_runs
               SET status = ?, updated_at = datetime('now', 'subsec')
               WHERE id = ?
               RETURNING {cols}"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(status.as_str())
            .bind(id)
            .fetch_optional(pool)
            .await
    }

    pub async fn cancel(pool: &SqlitePool, id: Uuid) -> Result<Option<Self>, sqlx::Error> {
        let query = format!(
            r#"UPDATE procedure_runs
               SET status = ?, updated_at = datetime('now', 'subsec')
               WHERE id = ? AND status = 'running'
               RETURNING {cols}"#,
            cols = Self::COLUMNS
        );
        sqlx::query_as::<_, Self>(&query)
            .bind(ProcedureRunStatus::Cancelled.as_str())
            .bind(id)
            .fetch_optional(pool)
            .await
    }
}
