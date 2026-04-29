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

/// What the procedure-graph view needs to render a state machine: each state
/// reduced to a node with a stable id + display kind, plus the directed edges
/// derived from `on_success` / `on_failure`. Layout is the client's job
/// (dagre); this just stays close to the YAML's logical shape.
#[derive(Debug, Serialize, Deserialize, TS)]
pub struct ProcedureGraphView {
    pub name: String,
    pub initial_state: String,
    pub nodes: Vec<ProcedureGraphNode>,
    pub edges: Vec<ProcedureGraphEdge>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct ProcedureGraphNode {
    pub id: String,
    pub kind: ProcedureNodeKind,
    /// Action kind (`create_session`, `follow_up`, `start_review`, `merge`)
    /// when the state has an action; absent for pure-gate or terminal states.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_kind: Option<String>,
    /// Gate kind (`deterministic`, `llm_judge`, `human`) when the state has a
    /// gate. UI surfaces this as a small badge alongside the action kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_kind: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ProcedureNodeKind {
    /// A working state with at least one of an action or a gate.
    Step,
    /// `terminal: success` — drawn in green by the UI.
    TerminalSuccess,
    /// `terminal: failure` — drawn in red.
    TerminalFailure,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct ProcedureGraphEdge {
    pub from: String,
    pub to: String,
    pub kind: ProcedureEdgeKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ProcedureEdgeKind {
    /// `on_success` transition.
    Success,
    /// `on_failure` transition.
    Failure,
}

/// Build a graph view of a procedure (parsed states + transitions) for the
/// React-Flow renderer. Pulls YAML from the same source as
/// [`get_procedure_source`] so built-ins and project-local both work.
pub async fn get_procedure_graph(
    State(deployment): State<DeploymentImpl>,
    Path((project_id, name)): Path<(Uuid, String)>,
) -> Result<ResponseJson<ApiResponse<ProcedureGraphView>>, ApiError> {
    let yaml = if let Some(yaml) = orchestration::builtin_procedure_yaml(&name) {
        yaml.to_string()
    } else {
        ProcedureRecord::find_by_name(&deployment.db().pool, project_id, &name)
            .await?
            .ok_or_else(|| ApiError::BadRequest(format!("procedure `{name}` not found")))?
            .yaml
    };
    let procedure = orchestration::load_from_yaml(&yaml)
        .map_err(|e| ApiError::BadRequest(format!("invalid procedure yaml: {e}")))?;
    Ok(ResponseJson(ApiResponse::success(graph_from_procedure(
        &procedure,
    ))))
}

fn graph_from_procedure(p: &orchestration::Procedure) -> ProcedureGraphView {
    let mut nodes = Vec::with_capacity(p.states.len());
    let mut edges = Vec::new();

    for (state_name, state) in &p.states {
        let kind = match state.terminal {
            Some(orchestration::Terminal::Success) => ProcedureNodeKind::TerminalSuccess,
            Some(orchestration::Terminal::Failure) => ProcedureNodeKind::TerminalFailure,
            None => ProcedureNodeKind::Step,
        };
        let action_kind = state.action.as_ref().map(|a| match a {
            orchestration::Action::CreateSession { .. } => "create_session".to_string(),
            orchestration::Action::FollowUp { .. } => "follow_up".to_string(),
            orchestration::Action::StartReview { .. } => "start_review".to_string(),
            orchestration::Action::Merge { .. } => "merge".to_string(),
        });
        let gate_kind = state.gate.as_ref().map(|g| match g {
            orchestration::Gate::Deterministic { .. } => "deterministic".to_string(),
            orchestration::Gate::LlmJudge { .. } => "llm_judge".to_string(),
            orchestration::Gate::Human { .. } => "human".to_string(),
        });
        nodes.push(ProcedureGraphNode {
            id: state_name.clone(),
            kind,
            action_kind,
            gate_kind,
        });

        if let Some(target) = &state.on_success {
            edges.push(ProcedureGraphEdge {
                from: state_name.clone(),
                to: target.clone(),
                kind: ProcedureEdgeKind::Success,
            });
        }
        if let Some(target) = &state.on_failure {
            edges.push(ProcedureGraphEdge {
                from: state_name.clone(),
                to: target.clone(),
                kind: ProcedureEdgeKind::Failure,
            });
        }
    }

    ProcedureGraphView {
        name: p.name.clone(),
        initial_state: p.initial_state.clone(),
        nodes,
        edges,
    }
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
        .route(
            "/projects/{project_id}/procedures/{name}/graph",
            get(get_procedure_graph),
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
    fn graph_from_builtin_smoke_success_has_terminal_and_step_nodes() {
        let yaml = orchestration::builtin_procedure_yaml("smoke_success").unwrap();
        let proc = orchestration::load_from_yaml(yaml).unwrap();
        let graph = graph_from_procedure(&proc);
        assert_eq!(graph.name, "smoke_success");
        assert_eq!(graph.initial_state, "plan");

        let plan = graph.nodes.iter().find(|n| n.id == "plan").unwrap();
        assert_eq!(plan.kind, ProcedureNodeKind::Step);
        assert_eq!(plan.action_kind.as_deref(), Some("create_session"));
        assert_eq!(plan.gate_kind.as_deref(), Some("deterministic"));

        let merge = graph.nodes.iter().find(|n| n.id == "merge").unwrap();
        // merge in smoke_success is a terminal-success state with a no-op
        // action that the runtime short-circuits over.
        assert_eq!(merge.kind, ProcedureNodeKind::TerminalSuccess);

        let failed = graph.nodes.iter().find(|n| n.id == "failed").unwrap();
        assert_eq!(failed.kind, ProcedureNodeKind::TerminalFailure);

        // plan -> review on success; plan -> failed on failure.
        assert!(
            graph.edges.iter().any(|e| e.from == "plan"
                && e.to == "review"
                && e.kind == ProcedureEdgeKind::Success)
        );
        assert!(
            graph.edges.iter().any(|e| e.from == "plan"
                && e.to == "failed"
                && e.kind == ProcedureEdgeKind::Failure)
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
