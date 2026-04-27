use axum::{Router, extract::State, response::Json as ResponseJson, routing::get};
use db::models::project::Project;
use deployment::Deployment;
use utils::response::ApiResponse;

use crate::{DeploymentImpl, error::ApiError};

pub async fn list_projects(
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<Vec<Project>>>, ApiError> {
    let projects = Project::find_all(&deployment.db().pool).await?;
    Ok(ResponseJson(ApiResponse::success(projects)))
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new().route("/projects", get(list_projects))
}
