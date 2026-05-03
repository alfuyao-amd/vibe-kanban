//! Per-session role labels for a workspace.
//!
//! Today VK shows every session in a workspace as a flat list. With the
//! lead-agent + procedure runtime layered on top, those sessions actually
//! play three distinct roles, and the UI needs to tell them apart:
//! - The project lead-agent session (one per workspace, persistent).
//! - Workers spawned by procedure runs (each carries the run id +
//!   procedure name so the UI can group them).
//! - Plain user sessions.
//!
//! This endpoint is the source-of-truth lookup: given a workspace id, return
//! a role per session id so the frontend can decorate the existing session
//! list without touching its rendering logic.

use std::collections::HashMap;

use axum::{Extension, Router, extract::State, response::Json as ResponseJson, routing::get};
use db::models::{
    procedure_run::ProcedureRun, project_lead_agent::ProjectLeadAgent, session::Session,
    workspace::Workspace,
};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum WorkspaceSessionRoleKind {
    /// The project's persistent lead-agent session bound to this workspace.
    LeadAgent,
    /// Spawned by a procedure run; carries `run_id` + `procedure_name`.
    ProcedureWorker,
    /// Anything else (user-created, ad-hoc tools).
    User,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct WorkspaceSessionRole {
    pub session_id: Uuid,
    pub role: WorkspaceSessionRoleKind,
    /// For `procedure_worker`: the run id this session belongs to. The UI
    /// uses this to group workers by run on the lead-agent dispatch page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
    /// For `procedure_worker`: the run's procedure name (e.g.
    /// `feature_with_tests`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub procedure_name: Option<String>,
    /// For `lead_agent`: the project this lead agent represents. Lets the UI
    /// link back to the project's procedures editor / runs page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
}

pub async fn get_session_roles(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<Vec<WorkspaceSessionRole>>>, ApiError> {
    let pool = &deployment.db().pool;
    let sessions = Session::find_by_workspace_id(pool, workspace.id).await?;
    let lead_records = ProjectLeadAgent::list_for_workspace(pool, workspace.id).await?;
    let runs = ProcedureRun::list_for_workspace(pool, workspace.id).await?;

    // session_id -> (run_id, procedure_name) for any session referenced by
    // an action-bearing entry in a run's state history. A session could
    // appear in multiple runs in theory (re-use across runs); keep the most
    // recent — runs come in created_at DESC order, so iterating in reverse
    // means the latest insert wins.
    let mut worker_lookup: HashMap<Uuid, (Uuid, String)> = HashMap::new();
    for run in runs.iter().rev() {
        for entry in run.state_history.0.iter() {
            if let Some(session_id_str) = &entry.session_id
                && let Ok(session_uuid) = Uuid::parse_str(session_id_str)
            {
                worker_lookup.insert(session_uuid, (run.id, run.procedure_name.clone()));
            }
        }
    }

    // session_id -> project_id for lead-agent sessions.
    let lead_lookup: HashMap<Uuid, Uuid> = lead_records
        .iter()
        .map(|r| (r.session_id, r.project_id))
        .collect();

    let roles: Vec<WorkspaceSessionRole> = sessions
        .iter()
        .map(|s| {
            if let Some(project_id) = lead_lookup.get(&s.id) {
                WorkspaceSessionRole {
                    session_id: s.id,
                    role: WorkspaceSessionRoleKind::LeadAgent,
                    run_id: None,
                    procedure_name: None,
                    project_id: Some(*project_id),
                }
            } else if let Some((run_id, procedure_name)) = worker_lookup.get(&s.id) {
                WorkspaceSessionRole {
                    session_id: s.id,
                    role: WorkspaceSessionRoleKind::ProcedureWorker,
                    run_id: Some(*run_id),
                    procedure_name: Some(procedure_name.clone()),
                    project_id: None,
                }
            } else {
                WorkspaceSessionRole {
                    session_id: s.id,
                    role: WorkspaceSessionRoleKind::User,
                    run_id: None,
                    procedure_name: None,
                    project_id: None,
                }
            }
        })
        .collect();

    Ok(ResponseJson(ApiResponse::success(roles)))
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new().route("/sessions/roles", get(get_session_roles))
}
