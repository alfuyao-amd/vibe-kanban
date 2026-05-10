use axum::{Json, Router, extract::State, response::Json as ResponseJson, routing::get};
use db::models::project::Project;
use deployment::Deployment;
use serde::Deserialize;
use ts_rs::TS;
use utils::response::ApiResponse;

use crate::{DeploymentImpl, error::ApiError};

pub async fn list_projects(
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<Vec<Project>>>, ApiError> {
    let projects = Project::find_all(&deployment.db().pool).await?;
    Ok(ResponseJson(ApiResponse::success(projects)))
}

#[derive(Debug, Deserialize, TS)]
pub struct CreateLocalProjectRequest {
    pub name: String,
    /// Optional default working dir for sessions / workspaces created under
    /// this project. Empty string is treated as `None`.
    #[serde(default)]
    pub default_agent_working_dir: Option<String>,
}

pub async fn create_local_project(
    State(deployment): State<DeploymentImpl>,
    Json(req): Json<CreateLocalProjectRequest>,
) -> Result<ResponseJson<ApiResponse<Project>>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("project name is required".into()));
    }
    let working_dir = req
        .default_agent_working_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let project = Project::create(&deployment.db().pool, name, working_dir).await?;
    Ok(ResponseJson(ApiResponse::success(project)))
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new().route("/projects", get(list_projects).post(create_local_project))
}
