use axum::{
    Json, Router,
    extract::{Path, State},
    response::Json as ResponseJson,
    routing::get,
};
use db::models::procedure::{ProcedureRecord, ProcedureSource};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{
    DeploymentImpl,
    error::ApiError,
    procedure_catalog,
    routes::procedure_runs::{ProcedureSourceLabel, ProcedureSummary},
};

#[derive(Debug, Deserialize, TS)]
pub struct UpsertProcedureRequest {
    pub yaml: String,
    /// Either `user` or `lead_agent`. Defaults to `user`.
    pub source: Option<String>,
}

/// Lists every procedure available to a project: built-ins (always) plus
/// project-local stored procedures, exposed as the same `ProcedureSummary`
/// shape used by `/api/procedures`. Each summary carries a `source` label so
/// the UI can mark built-ins as read-only and offer "Fork to project-local."
pub async fn list_for_project(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<Vec<ProcedureSummary>>>, ApiError> {
    let pool = &deployment.db().pool;
    let procedures = procedure_catalog::list_for_project(pool, project_id)
        .await
        .map_err(|e| ApiError::BadRequest(format!("failed to load procedures: {e}")))?;
    let stored = ProcedureRecord::list_for_project(pool, project_id).await?;

    let summaries = procedures
        .iter()
        .map(|p| {
            let label = label_for(&p.name, &stored);
            ProcedureSummary::from_procedure(p, label)
        })
        .collect::<Vec<_>>();
    Ok(ResponseJson(ApiResponse::success(summaries)))
}

fn label_for(name: &str, stored: &[ProcedureRecord]) -> ProcedureSourceLabel {
    // Built-ins always win on conflict (catalog already enforces this), so the
    // label is "builtin" whenever the name matches a bundled procedure.
    if orchestration::builtin_procedure_yaml(name).is_some() {
        return ProcedureSourceLabel::Builtin;
    }
    match stored
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.source.as_str())
    {
        Some("lead_agent") => ProcedureSourceLabel::LeadAgent,
        _ => ProcedureSourceLabel::User,
    }
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct ProcedureSourceView {
    pub name: String,
    pub yaml: String,
    pub source: ProcedureSourceLabel,
    /// True when this procedure is bundled with the binary and cannot be
    /// edited or deleted via the API. UIs should disable Save/Delete.
    pub read_only: bool,
}

/// Fetch the raw YAML body and source label for a single procedure (built-in
/// or project-local). Used by the procedure editor to populate the textarea
/// when editing or "forking" a built-in.
pub async fn get_procedure_source(
    State(deployment): State<DeploymentImpl>,
    Path((project_id, name)): Path<(Uuid, String)>,
) -> Result<ResponseJson<ApiResponse<ProcedureSourceView>>, ApiError> {
    if let Some(yaml) = orchestration::builtin_procedure_yaml(&name) {
        return Ok(ResponseJson(ApiResponse::success(ProcedureSourceView {
            name,
            yaml: yaml.to_string(),
            source: ProcedureSourceLabel::Builtin,
            read_only: true,
        })));
    }
    let record = ProcedureRecord::find_by_name(&deployment.db().pool, project_id, &name)
        .await?
        .ok_or_else(|| ApiError::BadRequest(format!("procedure `{name}` not found")))?;
    let label = match record.source.as_str() {
        "lead_agent" => ProcedureSourceLabel::LeadAgent,
        _ => ProcedureSourceLabel::User,
    };
    Ok(ResponseJson(ApiResponse::success(ProcedureSourceView {
        name: record.name,
        yaml: record.yaml,
        source: label,
        read_only: false,
    })))
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
            get(get_procedure_source).delete(delete_procedure),
        )
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn record(name: &str, source: &str) -> ProcedureRecord {
        ProcedureRecord {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            name: name.to_string(),
            version: 1,
            yaml: String::new(),
            source: source.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn label_for_built_in_wins_over_stored_row() {
        let stored = vec![record("feature_with_tests", "user")];
        assert_eq!(
            label_for("feature_with_tests", &stored),
            ProcedureSourceLabel::Builtin
        );
    }

    #[test]
    fn label_for_distinguishes_user_and_lead_agent_sources() {
        let stored = vec![record("ship_it", "user"), record("auto_yolo", "lead_agent")];
        assert_eq!(label_for("ship_it", &stored), ProcedureSourceLabel::User);
        assert_eq!(
            label_for("auto_yolo", &stored),
            ProcedureSourceLabel::LeadAgent
        );
    }

    #[test]
    fn label_for_falls_back_to_user_when_unknown() {
        // A name not in stored and not a built-in: defaults to User. The list
        // endpoint never produces this in practice (catalog ensures every
        // returned procedure is either stored or built-in), but the helper
        // should be defensive.
        let stored: Vec<ProcedureRecord> = vec![];
        assert_eq!(label_for("ghost", &stored), ProcedureSourceLabel::User);
    }
}
