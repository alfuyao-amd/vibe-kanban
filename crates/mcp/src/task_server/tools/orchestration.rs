use db::models::procedure_run::ProcedureRun;
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{McpMode, McpServer};
use crate::task_server::tools::ToolError;

#[derive(Debug, Deserialize)]
pub(super) struct WireProcedureSummary {
    pub name: String,
    pub version: u32,
    pub description: String,
    pub initial_state: String,
    pub match_hints: Vec<String>,
    pub params: Vec<WireProcedureParam>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct WireProcedureParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub required: bool,
    pub description: Option<String>,
}

impl Serialize for WireProcedureSummary {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ProcedureSummary", 6)?;
        s.serialize_field("name", &self.name)?;
        s.serialize_field("version", &self.version)?;
        s.serialize_field("description", &self.description)?;
        s.serialize_field("initial_state", &self.initial_state)?;
        s.serialize_field("match_hints", &self.match_hints)?;
        s.serialize_field("params", &self.params)?;
        s.end()
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SaveProcedureRequest {
    #[schemars(
        description = "Full YAML body of the procedure. Must include `name`, `version`, `initial_state`, and a `states` map."
    )]
    yaml: String,
}

#[derive(Debug, Serialize)]
struct SaveProcedurePayload {
    yaml: String,
    source: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeleteProcedureRequest {
    #[schemars(description = "Name of the project-local procedure to delete")]
    name: String,
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
        description = "List every procedure available to this project: built-ins (always available, immutable) plus any project-local procedures saved via save_procedure. Returns name, description, match_hints (when to use), and required/optional params."
    )]
    async fn list_procedures(&self) -> Result<CallToolResult, ErrorData> {
        let project_id = match self.resolve_procedure_project_id() {
            Ok(id) => id,
            Err(err) => return Ok(Self::tool_error(err)),
        };
        let url = self.url(&format!("/api/projects/{project_id}/procedures"));
        let procedures: Vec<crate::task_server::tools::orchestration::WireProcedureSummary> =
            match self.send_json(self.client.get(&url)).await {
                Ok(value) => value,
                Err(err) => return Ok(Self::tool_error(err)),
            };
        Self::success(&procedures)
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

    #[tool(
        description = "Approve a procedure run that is parked at a human-approval gate. The run resumes after approval. Returns the updated run."
    )]
    async fn approve_procedure_run(
        &self,
        Parameters(ProcedureRunIdRequest { run_id }): Parameters<ProcedureRunIdRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/procedure-runs/{}/approve", run_id));
        let run: ProcedureRun = match self.send_json(self.client.post(&url)).await {
            Ok(value) => value,
            Err(err) => return Ok(Self::tool_error(err)),
        };
        Self::success(&run)
    }

    #[tool(
        description = "Reject a procedure run that is parked at a human-approval gate. The run will fail. Returns the updated run."
    )]
    async fn reject_procedure_run(
        &self,
        Parameters(ProcedureRunIdRequest { run_id }): Parameters<ProcedureRunIdRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/procedure-runs/{}/reject", run_id));
        let run: ProcedureRun = match self.send_json(self.client.post(&url)).await {
            Ok(value) => value,
            Err(err) => return Ok(Self::tool_error(err)),
        };
        Self::success(&run)
    }

    #[tool(
        description = "Save a project-local procedure (YAML state machine). The YAML's `name` field is the procedure name; uploading with the same name updates and bumps version. Built-in names (feature_with_tests, smoke_success) cannot be overridden. The YAML must validate (initial_state must exist; transitions must reference declared states). Returns the saved record."
    )]
    async fn save_procedure(
        &self,
        Parameters(SaveProcedureRequest { yaml }): Parameters<SaveProcedureRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let project_id = match self.resolve_procedure_project_id() {
            Ok(id) => id,
            Err(err) => return Ok(Self::tool_error(err)),
        };
        let url = self.url(&format!("/api/projects/{project_id}/procedures"));
        let payload = SaveProcedurePayload {
            yaml,
            source: Some("lead_agent".to_string()),
        };
        let record: serde_json::Value =
            match self.send_json(self.client.post(&url).json(&payload)).await {
                Ok(value) => value,
                Err(err) => return Ok(Self::tool_error(err)),
            };
        Self::success(&record)
    }

    #[tool(
        description = "Delete a project-local procedure by name. Built-ins cannot be deleted; only procedures the lead agent or human created."
    )]
    async fn delete_procedure(
        &self,
        Parameters(DeleteProcedureRequest { name }): Parameters<DeleteProcedureRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let project_id = match self.resolve_procedure_project_id() {
            Ok(id) => id,
            Err(err) => return Ok(Self::tool_error(err)),
        };
        // Procedure names are identifier-shaped (alphanumeric + underscore by
        // convention) so URL-safe without encoding. If we ever allow names
        // with spaces or slashes this needs urlencoding.
        let url = self.url(&format!("/api/projects/{project_id}/procedures/{name}"));
        let _empty: serde_json::Value = match self.send_json(self.client.delete(&url)).await {
            Ok(value) => value,
            Err(err) => return Ok(Self::tool_error(err)),
        };
        Self::success(&serde_json::json!({ "deleted": name }))
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
