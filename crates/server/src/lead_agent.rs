//! Lead agent planner.
//!
//! Takes a natural-language goal and returns a picked `procedure_name` plus
//! `params`, by driving a one-shot Claude CLI session through VK's existing
//! executor stack. The picked plan is *not* run — the caller (typically the
//! UI) reviews it and decides whether to start a procedure run.

use std::path::PathBuf;

use db::models::{procedure_run::ProcedureRun, project_lead_agent::ProjectLeadAgent};
use orchestration::{Procedure, gates::extract_json_object};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use ts_rs::TS;
use uuid::Uuid;

use crate::{procedure_catalog, procedure_runtime::VkApiBackend};

/// Subdirectory under the asset root where per-project lead-agent MCP configs
/// are written.
const LEAD_AGENT_MCP_SUBDIR: &str = "lead_agent_mcp";
/// Server name used inside the generated MCP config — the agent's tool calls
/// surface as `mcp__vibe_kanban_project__<tool>`.
const LEAD_AGENT_MCP_SERVER_NAME: &str = "vibe_kanban_project";

/// Path to the per-project MCP config file the lead agent's Claude session
/// loads via `--mcp-config`. Does not check existence — pair with
/// [`ensure_mcp_config_file`] when you want it on disk.
pub fn mcp_config_file_for_project(project_id: Uuid) -> PathBuf {
    utils::assets::asset_dir()
        .join(LEAD_AGENT_MCP_SUBDIR)
        .join(format!("{project_id}.json"))
}

/// Build the JSON body for a project lead-agent's MCP config file. Picks the
/// command/args based on environment:
/// - `VIBE_LEAD_AGENT_MCP_BIN`: explicit override path to the binary.
/// - debug builds: locally-built `vibe-kanban-mcp` next to the running server,
///   if it exists.
/// - otherwise: `npx -y vibe-kanban@latest mcp ...`.
///
/// Forwards `VIBE_BACKEND_URL`, `BACKEND_PORT`, `HOST` to the spawned server
/// so it dials the same backend the lead agent is talking to.
pub fn build_mcp_config_body(project_id: Uuid) -> Value {
    let project_id_str = project_id.to_string();
    let env = forwarded_env_for_mcp();

    let server = if let Ok(bin) = std::env::var("VIBE_LEAD_AGENT_MCP_BIN") {
        serde_json::json!({
            "command": bin,
            "args": ["--mode", "project-orchestrator", "--project-id", project_id_str],
            "env": env,
        })
    } else if let Some(dev_bin) = debug_mcp_binary_path() {
        serde_json::json!({
            "command": dev_bin.display().to_string(),
            "args": ["--mode", "project-orchestrator", "--project-id", project_id_str],
            "env": env,
        })
    } else {
        serde_json::json!({
            "command": "npx",
            "args": [
                "-y",
                "vibe-kanban@latest",
                "mcp",
                "--mode",
                "project-orchestrator",
                "--project-id",
                project_id_str,
            ],
            "env": env,
        })
    };

    serde_json::json!({
        "mcpServers": { LEAD_AGENT_MCP_SERVER_NAME: server }
    })
}

fn forwarded_env_for_mcp() -> serde_json::Map<String, Value> {
    let mut env = serde_json::Map::new();
    for key in [
        "VIBE_BACKEND_URL",
        "BACKEND_PORT",
        "HOST",
        "MCP_HOST",
        "MCP_PORT",
    ] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), Value::String(val));
        }
    }
    env
}

fn debug_mcp_binary_path() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = exe_dir.join("vibe-kanban-mcp");
    candidate.exists().then_some(candidate)
}

/// Write the per-project MCP config file (overwriting any prior contents) and
/// return its path. Idempotent — safe to call on every lead-agent spawn so the
/// config picks up env changes.
pub fn ensure_mcp_config_file(project_id: Uuid) -> std::io::Result<PathBuf> {
    let path = mcp_config_file_for_project(project_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = build_mcp_config_body(project_id);
    std::fs::write(&path, serde_json::to_string_pretty(&body)?)?;
    Ok(path)
}

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
        .create_session_with_output("CLAUDE_CODE", &prompt, workspace_id, None)
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
         `approve_procedure_run`, `reject_procedure_run`. They are exposed by \
         the `vibe_kanban_project` MCP server already attached to this \
         session, so prefer them over shell commands or REST calls.\n\n",
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
    #[error("write mcp config: {0}")]
    McpConfig(#[from] std::io::Error),
}

/// Create a Claude session for the project lead agent in the given workspace,
/// seed it with the bootstrap prompt, and persist the (project, session)
/// mapping. Also writes the project-orchestrator MCP config file and attaches
/// it via `--mcp-config` so the agent's first turn can already call MCP tools.
/// Returns the new session info.
pub async fn start_session(
    backend: &VkApiBackend,
    pool: &SqlitePool,
    project_id: Uuid,
    workspace_id: Uuid,
) -> Result<LeadAgentSession, LeadAgentStartError> {
    let prompt = bootstrap_prompt(pool, project_id).await?;
    let mcp_config_path = ensure_mcp_config_file(project_id)?;
    let mcp_config_paths = Some(vec![mcp_config_path.display().to_string()]);
    let (session_id, _output) = backend
        .create_session_with_output("CLAUDE_CODE", &prompt, workspace_id, mcp_config_paths)
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

    #[test]
    fn mcp_config_body_pins_project_and_uses_orchestrator_mode() {
        let project_id = Uuid::new_v4();
        // Suppress env-driven branches so the assertion targets the default
        // shape rather than whatever is set on the developer's shell.
        // SAFETY: tests run single-threaded within a process for env isolation
        // is not guaranteed, but unsetting these only narrows the assertion.
        unsafe {
            std::env::remove_var("VIBE_LEAD_AGENT_MCP_BIN");
        }
        let body = build_mcp_config_body(project_id);

        let server = body
            .get("mcpServers")
            .and_then(|v| v.get("vibe_kanban_project"))
            .expect("vibe_kanban_project entry");
        let args: Vec<String> = server
            .get("args")
            .and_then(|v| v.as_array())
            .expect("args array")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();

        assert!(
            args.contains(&"--mode".to_string())
                && args.contains(&"project-orchestrator".to_string()),
            "args should select project-orchestrator mode: {args:?}"
        );
        assert!(
            args.contains(&"--project-id".to_string()) && args.contains(&project_id.to_string()),
            "args should pin the project_id: {args:?}"
        );
    }

    #[test]
    fn mcp_config_file_path_is_per_project() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let pa = mcp_config_file_for_project(a);
        let pb = mcp_config_file_for_project(b);
        assert_ne!(pa, pb);
        assert!(
            pa.to_string_lossy().contains(&a.to_string()),
            "path should embed project id: {pa:?}"
        );
    }
}
