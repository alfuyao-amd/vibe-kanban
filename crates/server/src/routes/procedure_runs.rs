use axum::{
    Json, Router,
    extract::{Path, State},
    response::Json as ResponseJson,
    routing::{get, post},
};
use db::models::{
    procedure_run::{CreateProcedureRun, ProcedureRun, ProcedureRunStatus},
    workspace::Workspace,
    workspace_repo::WorkspaceRepo,
};
use deployment::Deployment;
use orchestration::{Procedure, gates::ApprovalResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError, procedure_runtime};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProcedureSummary {
    pub name: String,
    pub version: u32,
    pub description: String,
    pub initial_state: String,
}

impl From<&Procedure> for ProcedureSummary {
    fn from(p: &Procedure) -> Self {
        Self {
            name: p.name.clone(),
            version: p.version,
            description: p.description.clone(),
            initial_state: p.initial_state.clone(),
        }
    }
}

#[derive(Debug, Deserialize, TS)]
pub struct StartProcedureRequest {
    pub procedure_name: String,
    #[ts(type = "Record<string, unknown>")]
    pub params: serde_json::Value,
    pub workspace_id: Option<Uuid>,
}

pub async fn list_procedures() -> Result<ResponseJson<ApiResponse<Vec<ProcedureSummary>>>, ApiError>
{
    let procedures = orchestration::builtin_procedures()
        .map_err(|e| ApiError::BadRequest(format!("failed to load procedures: {e}")))?;
    let summaries = procedures.iter().map(ProcedureSummary::from).collect();
    Ok(ResponseJson(ApiResponse::success(summaries)))
}

pub async fn start_procedure_run(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<StartProcedureRequest>,
) -> Result<ResponseJson<ApiResponse<ProcedureRun>>, ApiError> {
    let procedures = orchestration::builtin_procedures()
        .map_err(|e| ApiError::BadRequest(format!("failed to load procedures: {e}")))?;
    let procedure = procedures
        .into_iter()
        .find(|p| p.name == req.procedure_name)
        .ok_or_else(|| {
            ApiError::BadRequest(format!("unknown procedure `{}`", req.procedure_name))
        })?;

    let params =
        enrich_params_with_workspace(req.params.clone(), &deployment, req.workspace_id).await;

    let data = CreateProcedureRun {
        procedure_name: procedure.name.clone(),
        procedure_version: procedure.version as i64,
        initial_state: procedure.initial_state.clone(),
        params: params.clone(),
        workspace_id: req.workspace_id,
    };

    let run = ProcedureRun::create(&deployment.db().pool, project_id, &data).await?;

    procedure_runtime::spawn_procedure_run(deployment.db().pool.clone(), run.id, procedure, params);

    Ok(ResponseJson(ApiResponse::success(run)))
}

/// Resolve the workspace's worktree path and inject it into params under
/// `workspace.worktree_path` so YAML procedures can template
/// `{{workspace.worktree_path}}` (e.g. `cd {{workspace.worktree_path}} && {{test_command}}`).
/// Falls back silently if the workspace can't be found or has no worktree yet.
async fn enrich_params_with_workspace(
    params: Value,
    deployment: &DeploymentImpl,
    explicit_workspace_id: Option<Uuid>,
) -> Value {
    let workspace_id = explicit_workspace_id.or_else(|| {
        params
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
    });
    let Some(workspace_id) = workspace_id else {
        return params;
    };
    let workspace = match Workspace::find_by_id(&deployment.db().pool, workspace_id).await {
        Ok(Some(ws)) => ws,
        _ => return params,
    };
    let mut obj = match params {
        Value::Object(map) => map,
        other => {
            let mut m = serde_json::Map::new();
            if !matches!(other, Value::Null) {
                m.insert("value".to_string(), other);
            }
            m
        }
    };
    let workspace_root = workspace.container_ref.unwrap_or_default();

    // VK lays out attached repos as `<worktree_root>/<repo.name>`. For the
    // common single-repo case we point `workspace.worktree_path` at that
    // subdir so YAML templates can `cd {{workspace.worktree_path}} && {{cmd}}`
    // without knowing about the layout. The full root is also exposed as
    // `workspace.workspace_path` for procedures that need it.
    let repos = WorkspaceRepo::find_repos_for_workspace(&deployment.db().pool, workspace.id)
        .await
        .unwrap_or_default();
    let primary_repo_path = match repos.as_slice() {
        [single] if !workspace_root.is_empty() => format!("{workspace_root}/{}", single.name),
        _ => workspace_root.clone(),
    };

    let repos_json: Vec<Value> = repos
        .iter()
        .map(|r| {
            let path = if workspace_root.is_empty() {
                String::new()
            } else {
                format!("{workspace_root}/{}", r.name)
            };
            json!({
                "id": r.id.to_string(),
                "name": r.name,
                "path": path,
            })
        })
        .collect();

    obj.insert(
        "workspace".to_string(),
        json!({
            "id": workspace.id.to_string(),
            "workspace_path": workspace_root,
            "worktree_path": primary_repo_path,
            "branch": workspace.branch,
            "repos": repos_json,
        }),
    );
    // VkApiBackend::create_session reads params.workspace_id; mirror it here so
    // callers don't have to populate it themselves.
    obj.entry("workspace_id".to_string())
        .or_insert_with(|| Value::String(workspace.id.to_string()));
    Value::Object(obj)
}

pub async fn list_procedure_runs_for_project(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<Vec<ProcedureRun>>>, ApiError> {
    let runs = ProcedureRun::list_for_project(&deployment.db().pool, project_id).await?;
    Ok(ResponseJson(ApiResponse::success(runs)))
}

pub async fn list_all_procedure_runs(
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<Vec<ProcedureRun>>>, ApiError> {
    let runs = ProcedureRun::list_all(&deployment.db().pool).await?;
    Ok(ResponseJson(ApiResponse::success(runs)))
}

pub async fn get_procedure_run(
    State(deployment): State<DeploymentImpl>,
    Path(run_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<ProcedureRun>>, ApiError> {
    let run = ProcedureRun::find_by_id(&deployment.db().pool, run_id)
        .await?
        .ok_or_else(|| ApiError::BadRequest(format!("procedure run `{run_id}` not found")))?;
    Ok(ResponseJson(ApiResponse::success(run)))
}

pub async fn cancel_procedure_run(
    State(deployment): State<DeploymentImpl>,
    Path(run_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<ProcedureRun>>, ApiError> {
    let run = ProcedureRun::cancel(&deployment.db().pool, run_id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest(format!("procedure run `{run_id}` not found or not running"))
        })?;

    // If the run was parked on a human gate, wake it so the task exits.
    procedure_runtime::approvals()
        .signal(run_id, ApprovalResult::Rejected)
        .await;

    let _ = ProcedureRunStatus::Cancelled;
    Ok(ResponseJson(ApiResponse::success(run)))
}

pub async fn approve_procedure_run(
    State(deployment): State<DeploymentImpl>,
    Path(run_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<ProcedureRun>>, ApiError> {
    resolve_procedure_approval(&deployment, run_id, ApprovalResult::Approved).await
}

pub async fn reject_procedure_run(
    State(deployment): State<DeploymentImpl>,
    Path(run_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<ProcedureRun>>, ApiError> {
    resolve_procedure_approval(&deployment, run_id, ApprovalResult::Rejected).await
}

async fn resolve_procedure_approval(
    deployment: &DeploymentImpl,
    run_id: Uuid,
    result: ApprovalResult,
) -> Result<ResponseJson<ApiResponse<ProcedureRun>>, ApiError> {
    let run = ProcedureRun::find_by_id(&deployment.db().pool, run_id)
        .await?
        .ok_or_else(|| ApiError::BadRequest(format!("procedure run `{run_id}` not found")))?;
    if run.status != ProcedureRunStatus::AwaitingApproval.as_str() {
        return Err(ApiError::BadRequest(format!(
            "procedure run `{run_id}` is not awaiting approval (status={})",
            run.status
        )));
    }
    let delivered = procedure_runtime::approvals().signal(run_id, result).await;
    if !delivered {
        return Err(ApiError::BadRequest(format!(
            "no live approval channel for run `{run_id}`"
        )));
    }
    Ok(ResponseJson(ApiResponse::success(run)))
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/procedures", get(list_procedures))
        .route(
            "/projects/{project_id}/procedure-runs",
            get(list_procedure_runs_for_project).post(start_procedure_run),
        )
        .route("/procedure-runs", get(list_all_procedure_runs))
        .route("/procedure-runs/{run_id}", get(get_procedure_run))
        .route(
            "/procedure-runs/{run_id}/cancel",
            post(cancel_procedure_run),
        )
        .route(
            "/procedure-runs/{run_id}/approve",
            post(approve_procedure_run),
        )
        .route(
            "/procedure-runs/{run_id}/reject",
            post(reject_procedure_run),
        )
}
