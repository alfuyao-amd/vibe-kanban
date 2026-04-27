//! Lead agent planner.
//!
//! Takes a natural-language goal and returns a picked `procedure_name` plus
//! `params`, by driving a one-shot Claude CLI session through VK's existing
//! executor stack. The picked plan is *not* run — the caller (typically the
//! UI) reviews it and decides whether to start a procedure run.

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
