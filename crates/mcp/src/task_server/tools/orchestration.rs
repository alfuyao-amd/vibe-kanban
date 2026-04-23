use db::models::procedure_run::ProcedureRun;
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{McpMode, McpServer};
use crate::task_server::tools::ToolError;

#[derive(Debug, Serialize)]
struct ProcedureSummary {
    name: String,
    version: u32,
    description: String,
    match_hints: Vec<String>,
    required_params: Vec<String>,
    optional_params: Vec<String>,
}

impl From<&orchestration::Procedure> for ProcedureSummary {
    fn from(procedure: &orchestration::Procedure) -> Self {
        let (required, optional): (Vec<_>, Vec<_>) = procedure
            .triggers
            .params
            .iter()
            .partition(|(_, spec)| spec.required);
        Self {
            name: procedure.name.clone(),
            version: procedure.version,
            description: procedure.description.clone(),
            match_hints: procedure.triggers.match_hints.clone(),
            required_params: required.into_iter().map(|(k, _)| k.clone()).collect(),
            optional_params: optional.into_iter().map(|(k, _)| k.clone()).collect(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StartProcedureRequest {
    #[schemars(description = "Name of the procedure to start (see list_procedures)")]
    procedure_name: String,
    #[schemars(description = "Trigger params for the procedure (JSON object)")]
    params: serde_json::Value,
    #[schemars(description = "Optional workspace ID to associate with this run")]
    workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProcedureRunIdRequest {
    #[schemars(description = "Procedure run ID")]
    run_id: Uuid,
}

#[derive(Debug, Serialize)]
struct StartProcedurePayload {
    procedure_name: String,
    params: serde_json::Value,
    workspace_id: Option<Uuid>,
}

#[tool_router(router = orchestration_tools_router, vis = "pub")]
impl McpServer {
    #[tool(
        description = "List all registered procedures (YAML state-machine workflows) available to the lead agent. Returns name, description, match_hints (when to use), and required/optional params."
    )]
    async fn list_procedures(&self) -> Result<CallToolResult, ErrorData> {
        match orchestration::builtin_procedures() {
            Ok(procs) => {
                let summaries: Vec<ProcedureSummary> =
                    procs.iter().map(ProcedureSummary::from).collect();
                McpServer::success(&summaries)
            }
            Err(error) => {
                let details = error.to_string();
                McpServer::err("Failed to load procedures", Some(details.as_str()))
            }
        }
    }

    #[tool(
        description = "Start a new procedure run for the current project. Returns the created procedure_run row."
    )]
    async fn start_procedure(
        &self,
        Parameters(StartProcedureRequest {
            procedure_name,
            params,
            workspace_id,
        }): Parameters<StartProcedureRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let project_id = match self.resolve_procedure_project_id() {
            Ok(id) => id,
            Err(err) => return Ok(Self::tool_error(err)),
        };

        let payload = StartProcedurePayload {
            procedure_name,
            params,
            workspace_id,
        };

        let url = self.url(&format!("/api/projects/{}/procedure-runs", project_id));

        let run: ProcedureRun = match self.send_json(self.client.post(&url).json(&payload)).await {
            Ok(value) => value,
            Err(err) => return Ok(Self::tool_error(err)),
        };

        Self::success(&run)
    }

    #[tool(
        description = "Get the current state of a procedure run (status, current_state, state_history)."
    )]
    async fn get_procedure_state(
        &self,
        Parameters(ProcedureRunIdRequest { run_id }): Parameters<ProcedureRunIdRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/procedure-runs/{}", run_id));
        let run: ProcedureRun = match self.send_json(self.client.get(&url)).await {
            Ok(value) => value,
            Err(err) => return Ok(Self::tool_error(err)),
        };
        Self::success(&run)
    }

    #[tool(
        description = "Cancel a running procedure run. Returns the updated run, or an error if it is not currently running."
    )]
    async fn cancel_procedure(
        &self,
        Parameters(ProcedureRunIdRequest { run_id }): Parameters<ProcedureRunIdRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/procedure-runs/{}/cancel", run_id));
        let run: ProcedureRun = match self.send_json(self.client.post(&url)).await {
            Ok(value) => value,
            Err(err) => return Ok(Self::tool_error(err)),
        };
        Self::success(&run)
    }
}

impl McpServer {
    fn resolve_procedure_project_id(&self) -> Result<Uuid, ToolError> {
        match self.mode() {
            McpMode::ProjectOrchestrator { project_id } => Ok(*project_id),
            _ => self.resolve_project_id(None),
        }
    }
}
