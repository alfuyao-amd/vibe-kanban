use axum::{
    Json, Router,
    extract::{Path, State},
    response::Json as ResponseJson,
    routing::post,
};
use deployment::Deployment;
use serde::Deserialize;
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{
    DeploymentImpl,
    error::ApiError,
    lead_agent::{PickedPlan, PlanError, plan_for_goal},
    procedure_runtime::VkApiBackend,
};

#[derive(Debug, Deserialize, TS)]
pub struct PlanRequest {
    pub goal: String,
    pub workspace_id: Uuid,
}

pub async fn plan(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<PlanRequest>,
) -> Result<ResponseJson<ApiResponse<PickedPlan>>, ApiError> {
    let backend =
        VkApiBackend::from_env().map_err(|e| ApiError::BadRequest(format!("backend url: {e}")))?;
    let plan = plan_for_goal(
        &backend,
        &deployment.db().pool,
        project_id,
        &req.goal,
        req.workspace_id,
    )
    .await
    .map_err(plan_error_to_api)?;
    Ok(ResponseJson(ApiResponse::success(plan)))
}

fn plan_error_to_api(e: PlanError) -> ApiError {
    match e {
        PlanError::NoProcedures => ApiError::BadRequest(e.to_string()),
        PlanError::Backend(_) => ApiError::BadGateway(e.to_string()),
        PlanError::NoAssistantMessage
        | PlanError::NotJson(_)
        | PlanError::MissingProcedureName
        | PlanError::UnknownProcedure(_) => ApiError::BadRequest(e.to_string()),
    }
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new().route("/projects/{project_id}/lead-agent/plan", post(plan))
}
