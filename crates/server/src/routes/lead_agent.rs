use axum::{
    Json, Router,
    extract::{Path, State},
    response::Json as ResponseJson,
    routing::{get, post},
};
use db::models::project_lead_agent::ProjectLeadAgent;
use deployment::Deployment;
use serde::Deserialize;
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{
    DeploymentImpl,
    error::ApiError,
    lead_agent::{
        LeadAgentSession, LeadAgentStartError, PickedPlan, PlanError, plan_for_goal, start_session,
    },
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

#[derive(Debug, Deserialize, TS)]
pub struct StartLeadAgentRequest {
    pub workspace_id: Uuid,
}

pub async fn get_lead_agent(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<Option<LeadAgentSession>>>, ApiError> {
    let record = ProjectLeadAgent::find_for_project(&deployment.db().pool, project_id).await?;
    let view = record.as_ref().map(LeadAgentSession::from);
    Ok(ResponseJson(ApiResponse::success(view)))
}

pub async fn start_lead_agent(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<StartLeadAgentRequest>,
) -> Result<ResponseJson<ApiResponse<LeadAgentSession>>, ApiError> {
    let backend =
        VkApiBackend::from_env().map_err(|e| ApiError::BadRequest(format!("backend url: {e}")))?;
    let session = start_session(
        &backend,
        &deployment.db().pool,
        project_id,
        req.workspace_id,
    )
    .await
    .map_err(|e| match e {
        LeadAgentStartError::Backend(_) => ApiError::BadGateway(e.to_string()),
        LeadAgentStartError::Db(err) => ApiError::Database(err),
        LeadAgentStartError::InvalidSessionId(_) => ApiError::BadGateway(e.to_string()),
        LeadAgentStartError::McpConfig(err) => ApiError::Io(err),
    })?;
    Ok(ResponseJson(ApiResponse::success(session)))
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/projects/{project_id}/lead-agent/plan", post(plan))
        .route(
            "/projects/{project_id}/lead-agent",
            get(get_lead_agent).post(start_lead_agent),
        )
}
