use axum::{
    Json, Router,
    extract::{Path, State},
    response::Json as ResponseJson,
    routing::{delete, get},
};
use db::models::procedure::{ProcedureRecord, ProcedureSource};
use deployment::Deployment;
use serde::Deserialize;
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{
    DeploymentImpl, error::ApiError, procedure_catalog, routes::procedure_runs::ProcedureSummary,
};

#[derive(Debug, Deserialize, TS)]
pub struct UpsertProcedureRequest {
    pub yaml: String,
    /// Either `user` or `lead_agent`. Defaults to `user`.
    pub source: Option<String>,
}

/// Lists every procedure available to a project: built-ins (always) plus
/// project-local stored procedures, exposed as the same `ProcedureSummary`
/// shape used by `/api/procedures`.
pub async fn list_for_project(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<Vec<ProcedureSummary>>>, ApiError> {
    let procedures = procedure_catalog::list_for_project(&deployment.db().pool, project_id)
        .await
        .map_err(|e| ApiError::BadRequest(format!("failed to load procedures: {e}")))?;
    let summaries = procedures
        .iter()
        .map(ProcedureSummary::from)
        .collect::<Vec<_>>();
    Ok(ResponseJson(ApiResponse::success(summaries)))
}

/// Validate the YAML, then upsert it as a project-local procedure. The
/// procedure name comes from the YAML body, not the URL.
pub async fn upsert_procedure(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<UpsertProcedureRequest>,
) -> Result<ResponseJson<ApiResponse<ProcedureRecord>>, ApiError> {
    let parsed = orchestration::load_from_yaml(&req.yaml)
        .map_err(|e| ApiError::BadRequest(format!("invalid procedure yaml: {e}")))?;

    // Built-in names are immutable.
    let builtins = orchestration::builtin_procedures()
        .map_err(|e| ApiError::BadRequest(format!("load builtins: {e}")))?;
    if builtins.iter().any(|b| b.name == parsed.name) {
        return Err(ApiError::Conflict(format!(
            "procedure name `{}` is a built-in and cannot be overridden",
            parsed.name
        )));
    }

    let source = match req.source.as_deref() {
        Some("lead_agent") => ProcedureSource::LeadAgent,
        _ => ProcedureSource::User,
    };

    let record = ProcedureRecord::upsert(
        &deployment.db().pool,
        project_id,
        &parsed.name,
        &req.yaml,
        source,
    )
    .await?;
    Ok(ResponseJson(ApiResponse::success(record)))
}

pub async fn delete_procedure(
    State(deployment): State<DeploymentImpl>,
    Path((project_id, name)): Path<(Uuid, String)>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    let removed = ProcedureRecord::delete(&deployment.db().pool, project_id, &name).await?;
    if !removed {
        return Err(ApiError::BadRequest(format!(
            "no project-local procedure named `{name}` (built-ins cannot be deleted)"
        )));
    }
    Ok(ResponseJson(ApiResponse::success(())))
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route(
            "/projects/{project_id}/procedures",
            get(list_for_project).post(upsert_procedure),
        )
        .route(
            "/projects/{project_id}/procedures/{name}",
            delete(delete_procedure),
        )
}
