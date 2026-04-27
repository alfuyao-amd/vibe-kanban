//! Lead agent planner.
//!
//! Takes a natural-language goal and returns a picked `procedure_name` plus
//! `params`, by driving a one-shot Claude CLI session through VK's existing
//! executor stack. The picked plan is *not* run — the caller (typically the
//! UI) reviews it and decides whether to start a procedure run.

use db::models::{procedure_run::ProcedureRun, project_lead_agent::ProjectLeadAgent};
use orchestration::{Procedure, gates::extract_json_object};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use ts_rs::TS;
use uuid::Uuid;

use crate::{procedure_catalog, procedure_runtime::VkApiBackend};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PickedPlan {
    pub procedure_name: String,
    #[ts(type = "Record<string, unknown>")]
    pub params: Value,
    /// The raw assistant message; useful for surfacing reasoning to the user
    /// when the JSON the planner returned is missing fields.
    pub raw_assistant_message: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("no built-in procedures available")]
    NoProcedures,
    #[error("backend: {0}")]
    Backend(String),
    #[error("planner returned no assistant message")]
    NoAssistantMessage,
    #[error("planner output is not parseable JSON: {0}")]
    NotJson(String),
    #[error("planner JSON missing field `procedure_name`")]
    MissingProcedureName,
    #[error("planner picked unknown procedure `{0}`")]
    UnknownProcedure(String),
}

/// Build the planning prompt describing each available procedure with its
/// trigger params.
fn build_prompt(goal: &str, procedures: &[Procedure]) -> String {
    let mut s = String::new();
    s.push_str("You are picking a procedure for the user's goal.\n\n");
    s.push_str("Available procedures:\n");
    for proc in procedures {
        s.push_str(&format!("- {}", proc.name));
        if !proc.description.is_empty() {
            s.push_str(&format!(" — {}", proc.description.trim()));
        }
        s.push('\n');
        if !proc.triggers.params.is_empty() {
            s.push_str("    params:\n");
            for (name, spec) in &proc.triggers.params {
                let req = if spec.required {
                    "required"
                } else {
                    "optional"
                };
                let ty = format!("{:?}", spec.ty).to_lowercase();
                s.push_str(&format!("      - {name} ({ty}, {req})"));
                if let Some(desc) = &spec.description {
                    s.push_str(&format!(": {}", desc.trim()));
                }
                s.push('\n');
            }
        }
        if !proc.triggers.match_hints.is_empty() {
            s.push_str(&format!(
                "    use when: {}\n",
                proc.triggers.match_hints.join(", ")
            ));
        }
    }
    s.push('\n');
    s.push_str(&format!("User goal:\n{goal}\n\n"));
    s.push_str(
        "Respond with a single JSON object and nothing else: \
         {\"procedure_name\": \"...\", \"params\": {...}}. \
         Include all required params for the picked procedure. \
         Do not include `workspace_id` (the caller injects it). \
         Do not run any commands or modify files; only emit the JSON.\n",
    );
    s
}

/// Drive a one-shot Claude session in the given workspace and return the
/// picked plan.
pub async fn plan_for_goal(
    backend: &VkApiBackend,
    pool: &SqlitePool,
    project_id: Uuid,
    goal: &str,
    workspace_id: Uuid,
) -> Result<PickedPlan, PlanError> {
    let procedures = procedure_catalog::list_for_project(pool, project_id)
        .await
        .map_err(|e| PlanError::Backend(format!("load procedures: {e}")))?;
    if procedures.is_empty() {
        return Err(PlanError::NoProcedures);
    }

    let prompt = build_prompt(goal, &procedures);

    let (_session_id, output) = backend
        .create_session_with_output("CLAUDE_CODE", &prompt, workspace_id)
        .await
        .map_err(|e| PlanError::Backend(e.to_string()))?;

    let raw = output
        .last_assistant_message
        .ok_or(PlanError::NoAssistantMessage)?;
    let response = extract_json_object(&raw)
        .ok_or_else(|| PlanError::NotJson(raw.chars().take(200).collect()))?;

    let procedure_name = response
        .get("procedure_name")
        .and_then(|v| v.as_str())
        .ok_or(PlanError::MissingProcedureName)?
        .to_string();

    if !procedures.iter().any(|p| p.name == procedure_name) {
        return Err(PlanError::UnknownProcedure(procedure_name));
    }

    let params = response
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    Ok(PickedPlan {
        procedure_name,
        params,
        raw_assistant_message: Some(raw),
    })
}

/// Build the system prompt that bootstraps a fresh project lead-agent session.
/// The agent gets: its role, the YAML procedure schema, the current catalog,
/// recent runs, and the briefing convention (auto-save + plain-language
/// summary, never assume the user reads YAML).
pub async fn bootstrap_prompt(pool: &SqlitePool, project_id: Uuid) -> Result<String, sqlx::Error> {
    let procedures = procedure_catalog::list_for_project(pool, project_id)
        .await
        .unwrap_or_default();
    let runs = ProcedureRun::list_for_project(pool, project_id)
        .await
        .unwrap_or_default();

    let mut s = String::new();
    s.push_str(
        "You are the project lead agent for this Vibe Kanban project.\n\n\
         Your job: when the human describes what they want, decide whether to:\n\
         (a) start an existing procedure run, or\n\
         (b) author a new procedure (YAML state machine) and then run it.\n\n\
         When you author a procedure, save it immediately via the \
         `save_procedure` MCP tool — do not show YAML to the human and ask \
         them to read it. After saving, brief the human in plain English: \
         \"I added a procedure called X. It does step 1, then step 2, then \
         step 3. Want me to run it?\"\n\n\
         Use other tools as needed: `list_procedures`, `start_procedure`, \
         `get_procedure_state`, `cancel_procedure`, `delete_procedure`, \
         `approve_procedure_run`, `reject_procedure_run`.\n\n",
    );

    s.push_str("--- Procedure YAML schema ---\n");
    s.push_str(
        "name: <snake_case>\n\
         version: 1\n\
         description: <one paragraph>\n\
         triggers:\n  match_hints: [<short phrases users might say>]\n  params:\n    \
         <param_name>:\n      type: string|integer|boolean|array\n      \
         required: true|false\n      description: <what this is for>\n\
         initial_state: <name of first state>\n\
         states:\n  <state_name>:\n    action:\n      kind: \
         create_session|follow_up|start_review|merge\n      executor: \
         CLAUDE_CODE  # one of CLAUDE_CODE, CODEX, GEMINI, etc.\n      \
         session_ref: <previous state name (for follow_up/start_review/merge)>\n      \
         prompt: |\n        <prompt with {{param}} placeholders>\n    gate: \
         # optional\n      kind: deterministic|llm_judge|human\n      run: \
         \"<shell cmd, deterministic only>\"\n      pass_when: \
         \"exit_code == 0\" | \"response.<path> == 'value'\"\n      prompt: \
         \"<for human gates>\"\n    max_attempts: 1  # optional, defaults to 1\n    \
         on_success: <next state>\n    on_failure: <next state>\n  <terminal_name>:\n    \
         terminal: success|failure\n\n\
         Rules:\n\
         - initial_state must reference a declared state.\n\
         - on_success / on_failure must reference declared states.\n\
         - terminal states have no action/gate/transitions.\n\
         - llm_judge gates parse the preceding action's last assistant \
         message as JSON and evaluate `pass_when` against `response.<path>`.\n\
         - For commands that need to run inside a workspace, prefix with \
         `cd {{workspace.worktree_path}} && ...`.\n\n",
    );

    if procedures.is_empty() {
        s.push_str("--- Current procedures ---\n(none yet — author one with save_procedure when needed)\n\n");
    } else {
        s.push_str("--- Current procedures available in this project ---\n");
        for proc in &procedures {
            s.push_str(&format!(
                "- {} (v{}): {}\n",
                proc.name,
                proc.version,
                proc.description.trim()
            ));
        }
        s.push('\n');
    }

    if !runs.is_empty() {
        s.push_str("--- Recent runs (newest first, up to 10) ---\n");
        for run in runs.iter().take(10) {
            s.push_str(&format!(
                "- {} [{}] {} (id={})\n",
                run.procedure_name, run.status, run.current_state, run.id
            ));
        }
        s.push('\n');
    }

    s.push_str(
        "Be concise. Confirm before destructive actions. When you save or \
         change a procedure, immediately summarize what changed in plain \
         English. When a human-approval gate is hit, surface the prompt \
         and let the human decide via Approve / Reject in the UI (or call \
         approve_procedure_run if they explicitly authorize you).\n",
    );

    Ok(s)
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct LeadAgentSession {
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub workspace_id: Uuid,
}

impl From<&ProjectLeadAgent> for LeadAgentSession {
    fn from(record: &ProjectLeadAgent) -> Self {
        Self {
            project_id: record.project_id,
            session_id: record.session_id,
            workspace_id: record.workspace_id,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LeadAgentStartError {
    #[error("backend: {0}")]
    Backend(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("invalid session id from backend: {0}")]
    InvalidSessionId(String),
}

/// Create a Claude session for the project lead agent in the given workspace,
/// seed it with the bootstrap prompt, and persist the (project, session)
/// mapping. Returns the new session info.
pub async fn start_session(
    backend: &VkApiBackend,
    pool: &SqlitePool,
    project_id: Uuid,
    workspace_id: Uuid,
) -> Result<LeadAgentSession, LeadAgentStartError> {
    let prompt = bootstrap_prompt(pool, project_id).await?;
    let (session_id, _output) = backend
        .create_session_with_output("CLAUDE_CODE", &prompt, workspace_id)
        .await
        .map_err(|e| LeadAgentStartError::Backend(e.to_string()))?;
    let session_uuid = Uuid::parse_str(&session_id.0)
        .map_err(|e| LeadAgentStartError::InvalidSessionId(format!("{e}")))?;
    let record = ProjectLeadAgent::upsert(pool, project_id, session_uuid, workspace_id).await?;
    Ok((&record).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_procedure(name: &str) -> Procedure {
        let yaml = format!(
            r#"
name: {name}
version: 1
description: test
initial_state: done
states:
  done:
    terminal: success
"#
        );
        orchestration::load_from_yaml(&yaml).unwrap()
    }

    #[test]
    fn build_prompt_lists_procedures_and_params() {
        let p = make_procedure("smoke");
        let prompt = build_prompt("add a thing", &[p]);
        assert!(prompt.contains("smoke"));
        assert!(prompt.contains("add a thing"));
        assert!(prompt.contains("procedure_name"));
    }
}
