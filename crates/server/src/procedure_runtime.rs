use std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, OnceLock},
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use db::models::procedure_run::{
    ProcedureRun, ProcedureRunStatus, StateHistoryEntry as DbEntry, StateOutcome,
};
use executors::{executors::BaseCodingAgent, profile::ExecutorConfig};
use orchestration::{
    ExecutionBackend, Procedure,
    gates::{
        ApprovalResult, AutoApprove, ContextLlmJudge, DeterministicGateEvaluator, GateError,
        HumanApprovalSource, HumanGateEvaluator,
    },
    state_machine::{
        BackendError, ExecutionId, ExecutionOutput, GateEvaluators, MergeOutcome,
        ProcedureExecutor, RunId, RunOutcome, RunStore, SessionId, StateEvent,
        StateHistoryEntry as OrchEntry,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use uuid::Uuid;

pub struct DbRunStore {
    pool: SqlitePool,
}

impl DbRunStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    async fn append_history(&self, run_id: Uuid, to_state: &str, entry: DbEntry) {
        let mut current = match ProcedureRun::find_by_id(&self.pool, run_id).await {
            Ok(Some(r)) => r,
            _ => return,
        };
        current.state_history.0.push(entry);
        let history_json = serde_json::to_string(&current.state_history.0).unwrap_or("[]".into());
        let _ = sqlx::query(
            r#"UPDATE procedure_runs
               SET state_history = ?, current_state = ?, updated_at = datetime('now', 'subsec')
               WHERE id = ?"#,
        )
        .bind(history_json)
        .bind(to_state)
        .bind(run_id)
        .execute(&self.pool)
        .await;
    }
}

fn convert_entry(from: &str, to: &str, orch: &OrchEntry) -> DbEntry {
    let now = Utc::now();
    let (outcome, gate_summary) = match &orch.event {
        StateEvent::GateEvaluated { passed, summary } => (
            Some(if *passed {
                StateOutcome::Success
            } else {
                StateOutcome::Failure
            }),
            Some(summary.clone()),
        ),
        StateEvent::ActionCompleted { action_kind, .. } => (
            Some(if from == to {
                StateOutcome::Failure
            } else {
                StateOutcome::Success
            }),
            Some(format!("action: {action_kind}")),
        ),
        StateEvent::MaxAttemptsExhausted { attempts } => (
            Some(StateOutcome::Failure),
            Some(format!("max_attempts exhausted after {attempts} tries")),
        ),
    };
    DbEntry {
        state: orch.state.clone(),
        entered_at: now,
        exited_at: Some(now),
        outcome,
        gate_summary,
        attempt: orch.attempt,
    }
}

#[async_trait]
impl RunStore for DbRunStore {
    async fn persist_transition(
        &self,
        run_id: RunId,
        from_state: &str,
        to_state: &str,
        entry: OrchEntry,
    ) {
        let db_entry = convert_entry(from_state, to_state, &entry);
        self.append_history(run_id.0, to_state, db_entry).await;
    }

    async fn set_terminal(&self, run_id: RunId, outcome: RunOutcome) {
        let status = match outcome {
            RunOutcome::Success => ProcedureRunStatus::Succeeded,
            RunOutcome::Failure => ProcedureRunStatus::Failed,
        };
        let _ = ProcedureRun::update_status(&self.pool, run_id.0, status).await;
    }
}

/// Per-process registry mapping a procedure run to its pending-approval channel.
/// HTTP approve/reject handlers push into the sender; the run's HumanApprovalSource
/// reads from the receiver.
#[derive(Clone, Default)]
pub struct ProcedureApprovalRegistry {
    inner: Arc<Mutex<HashMap<Uuid, mpsc::Sender<ApprovalResult>>>>,
}

impl ProcedureApprovalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, run_id: Uuid, tx: mpsc::Sender<ApprovalResult>) {
        self.inner.lock().await.insert(run_id, tx);
    }

    pub async fn deregister(&self, run_id: Uuid) {
        self.inner.lock().await.remove(&run_id);
    }

    pub async fn signal(&self, run_id: Uuid, result: ApprovalResult) -> bool {
        let sender = self.inner.lock().await.get(&run_id).cloned();
        match sender {
            Some(tx) => tx.send(result).await.is_ok(),
            None => false,
        }
    }
}

static APPROVALS: OnceLock<ProcedureApprovalRegistry> = OnceLock::new();

pub fn approvals() -> &'static ProcedureApprovalRegistry {
    APPROVALS.get_or_init(ProcedureApprovalRegistry::new)
}

/// HumanApprovalSource that flips the DB row to awaiting_approval while it
/// waits on the run's mpsc channel. Restores status when a signal arrives.
struct RegistryApproval {
    run_id: Uuid,
    pool: SqlitePool,
    rx: Mutex<mpsc::Receiver<ApprovalResult>>,
}

impl RegistryApproval {
    async fn install(run_id: Uuid, pool: SqlitePool) -> Self {
        let (tx, rx) = mpsc::channel(4);
        approvals().register(run_id, tx).await;
        Self {
            run_id,
            pool,
            rx: Mutex::new(rx),
        }
    }
}

#[async_trait]
impl HumanApprovalSource for RegistryApproval {
    async fn wait_for_approval(&self, prompt: &str) -> Result<ApprovalResult, GateError> {
        if let Err(err) = ProcedureRun::set_awaiting_approval(&self.pool, self.run_id, prompt).await
        {
            tracing::warn!(run_id = %self.run_id, %err, "failed to set awaiting_approval");
        }
        let result = {
            let mut guard = self.rx.lock().await;
            guard.recv().await.ok_or(GateError::ApprovalChannelClosed)?
        };
        if let Err(err) = ProcedureRun::clear_awaiting_approval(&self.pool, self.run_id).await {
            tracing::warn!(run_id = %self.run_id, %err, "failed to clear awaiting_approval");
        }
        Ok(result)
    }
}

pub struct StubBackend;

#[async_trait]
impl ExecutionBackend for StubBackend {
    async fn create_session(
        &self,
        _executor: &str,
        _prompt: &str,
        _params: &Value,
    ) -> Result<SessionId, BackendError> {
        Ok(SessionId(Uuid::new_v4().to_string()))
    }

    async fn follow_up(
        &self,
        _session_id: SessionId,
        _executor: Option<&str>,
        _prompt: &str,
    ) -> Result<ExecutionId, BackendError> {
        Ok(ExecutionId(Uuid::new_v4().to_string()))
    }

    async fn start_review(
        &self,
        _session_id: SessionId,
        _executor: &str,
        _prompt: &str,
    ) -> Result<ExecutionId, BackendError> {
        Ok(ExecutionId(Uuid::new_v4().to_string()))
    }

    async fn merge(&self, _session_id: SessionId) -> Result<MergeOutcome, BackendError> {
        Ok(MergeOutcome::Merged {
            commit_sha: "stub-merge-commit".to_string(),
        })
    }

    async fn await_completion(
        &self,
        _execution_id: ExecutionId,
    ) -> Result<ExecutionOutput, BackendError> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(ExecutionOutput {
            stdout: "stub".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            // Valid JSON so that ContextLlmJudge can parse it when a review
            // gate runs under the stub backend (e.g. smoke_success).
            last_assistant_message: Some(
                r#"{"verdict":"pass","feedback":"stub approved"}"#.to_string(),
            ),
        })
    }
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    #[allow(dead_code)]
    success: bool,
    data: Option<T>,
    #[allow(dead_code)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionView {
    id: Uuid,
    workspace_id: Uuid,
    executor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceRepoView {
    repo: RepoView,
}

#[derive(Debug, Deserialize)]
struct RepoView {
    id: Uuid,
}

#[derive(Debug, Serialize)]
struct MergeWorkspaceBody {
    repo_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct ExecProcView {
    #[allow(dead_code)]
    id: Uuid,
    status: String,
    exit_code: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LastAssistantMessageView {
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateSessionBody<'a> {
    workspace_id: Uuid,
    executor: Option<&'a str>,
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct FollowUpBody {
    prompt: String,
    executor_config: ExecutorConfig,
}

#[derive(Debug, Serialize)]
struct StartReviewBody {
    executor_config: ExecutorConfig,
    additional_prompt: Option<String>,
    use_all_workspace_commits: bool,
}

pub struct VkApiBackend {
    client: reqwest::Client,
    base_url: String,
    poll_interval: Duration,
}

impl VkApiBackend {
    pub fn new(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            poll_interval: Duration::from_secs(2),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        let base_url = resolve_base_url()?;
        Ok(Self::new(base_url))
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn unwrap_envelope<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
        context: &str,
    ) -> Result<T, BackendError> {
        let envelope = Self::check_envelope::<T>(resp, context).await?;
        envelope
            .data
            .ok_or_else(|| BackendError::Generic(format!("{context}: response had no data")))
    }

    async fn check_envelope<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
        context: &str,
    ) -> Result<ApiEnvelope<T>, BackendError> {
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| BackendError::Generic(format!("{context}: read body: {e}")))?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            return Err(BackendError::Generic(format!(
                "{context}: HTTP {status}: {body}"
            )));
        }
        serde_json::from_slice(&bytes).map_err(|e| {
            BackendError::Generic(format!(
                "{context}: decode: {e}: body={}",
                String::from_utf8_lossy(&bytes)
            ))
        })
    }

    async fn get_session(&self, session_id: &Uuid) -> Result<SessionView, BackendError> {
        let url = self.url(&format!("/api/sessions/{session_id}"));
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| BackendError::Generic(format!("GET {url}: {e}")))?;
        Self::unwrap_envelope(resp, &format!("GET {url}")).await
    }

    fn build_executor_config(executor: &str) -> Result<ExecutorConfig, BackendError> {
        let base = BaseCodingAgent::from_str(executor)
            .map_err(|e| BackendError::Generic(format!("unknown executor `{executor}`: {e}")))?;
        Ok(ExecutorConfig::new(base))
    }
}

fn resolve_base_url() -> Result<String, String> {
    if let Ok(url) = std::env::var("VIBE_BACKEND_URL") {
        return Ok(url);
    }
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("BACKEND_PORT")
        .or_else(|_| std::env::var("PORT"))
        .map_err(|_| "neither VIBE_BACKEND_URL nor BACKEND_PORT is set".to_string())?;
    Ok(format!("http://{host}:{port}"))
}

#[async_trait]
impl ExecutionBackend for VkApiBackend {
    async fn create_session(
        &self,
        executor: &str,
        prompt: &str,
        params: &Value,
    ) -> Result<SessionId, BackendError> {
        let workspace_id = params
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                BackendError::Generic(
                    "params.workspace_id is required for VkApiBackend create_session".to_string(),
                )
            })
            .and_then(|s| {
                Uuid::parse_str(s)
                    .map_err(|e| BackendError::Generic(format!("invalid workspace_id `{s}`: {e}")))
            })?;

        let create_url = self.url("/api/sessions");
        let resp = self
            .client
            .post(&create_url)
            .json(&CreateSessionBody {
                workspace_id,
                executor: Some(executor),
                name: Some("procedure run".to_string()),
            })
            .send()
            .await
            .map_err(|e| BackendError::Generic(format!("POST {create_url}: {e}")))?;
        let session: SessionView =
            Self::unwrap_envelope(resp, &format!("POST {create_url}")).await?;

        let followup_url = self.url(&format!("/api/sessions/{}/follow-up", session.id));
        let followup_resp = self
            .client
            .post(&followup_url)
            .json(&FollowUpBody {
                prompt: prompt.to_string(),
                executor_config: Self::build_executor_config(executor)?,
            })
            .send()
            .await
            .map_err(|e| BackendError::Generic(format!("POST {followup_url}: {e}")))?;
        let process: ExecProcView =
            Self::unwrap_envelope(followup_resp, &format!("POST {followup_url}")).await?;

        self.await_completion(ExecutionId(process.id.to_string()))
            .await?;
        Ok(SessionId(session.id.to_string()))
    }

    async fn follow_up(
        &self,
        session_id: SessionId,
        executor: Option<&str>,
        prompt: &str,
    ) -> Result<ExecutionId, BackendError> {
        let sid = Uuid::parse_str(&session_id.0)
            .map_err(|e| BackendError::Generic(format!("invalid session_id: {e}")))?;

        let executor_str = match executor {
            Some(e) => e.to_string(),
            None => self.get_session(&sid).await?.executor.ok_or_else(|| {
                BackendError::Generic(format!("session {sid} has no executor set"))
            })?,
        };

        let url = self.url(&format!("/api/sessions/{sid}/follow-up"));
        let resp = self
            .client
            .post(&url)
            .json(&FollowUpBody {
                prompt: prompt.to_string(),
                executor_config: Self::build_executor_config(&executor_str)?,
            })
            .send()
            .await
            .map_err(|e| BackendError::Generic(format!("POST {url}: {e}")))?;
        let process: ExecProcView = Self::unwrap_envelope(resp, &format!("POST {url}")).await?;
        Ok(ExecutionId(process.id.to_string()))
    }

    async fn start_review(
        &self,
        session_id: SessionId,
        executor: &str,
        prompt: &str,
    ) -> Result<ExecutionId, BackendError> {
        let sid = Uuid::parse_str(&session_id.0)
            .map_err(|e| BackendError::Generic(format!("invalid session_id: {e}")))?;
        let url = self.url(&format!("/api/sessions/{sid}/review"));
        let resp = self
            .client
            .post(&url)
            .json(&StartReviewBody {
                executor_config: Self::build_executor_config(executor)?,
                additional_prompt: if prompt.is_empty() {
                    None
                } else {
                    Some(prompt.to_string())
                },
                use_all_workspace_commits: false,
            })
            .send()
            .await
            .map_err(|e| BackendError::Generic(format!("POST {url}: {e}")))?;
        let process: ExecProcView = Self::unwrap_envelope(resp, &format!("POST {url}")).await?;
        Ok(ExecutionId(process.id.to_string()))
    }

    async fn merge(&self, session_id: SessionId) -> Result<MergeOutcome, BackendError> {
        let sid = Uuid::parse_str(&session_id.0)
            .map_err(|e| BackendError::Generic(format!("invalid session_id: {e}")))?;
        let session = self.get_session(&sid).await?;
        let workspace_id = session.workspace_id;

        let repos_url = self.url(&format!("/api/workspaces/{workspace_id}/repos"));
        let resp = self
            .client
            .get(&repos_url)
            .send()
            .await
            .map_err(|e| BackendError::Generic(format!("GET {repos_url}: {e}")))?;
        let repos: Vec<WorkspaceRepoView> =
            Self::unwrap_envelope(resp, &format!("GET {repos_url}")).await?;

        let repo_id = match repos.as_slice() {
            [single] => single.repo.id,
            [] => {
                return Err(BackendError::Generic(format!(
                    "workspace {workspace_id} has no repos to merge"
                )));
            }
            many => {
                return Err(BackendError::Generic(format!(
                    "workspace {workspace_id} has {} repos; merge expects exactly one. \
                     Multi-repo merge is not yet supported.",
                    many.len()
                )));
            }
        };

        let merge_url = self.url(&format!("/api/workspaces/{workspace_id}/merge"));
        let resp = self
            .client
            .post(&merge_url)
            .json(&MergeWorkspaceBody { repo_id })
            .send()
            .await
            .map_err(|e| BackendError::Generic(format!("POST {merge_url}: {e}")))?;
        let _ =
            Self::check_envelope::<serde_json::Value>(resp, &format!("POST {merge_url}")).await?;

        Ok(MergeOutcome::Merged {
            commit_sha: format!("workspace:{workspace_id}/repo:{repo_id}"),
        })
    }

    async fn await_completion(
        &self,
        execution_id: ExecutionId,
    ) -> Result<ExecutionOutput, BackendError> {
        let eid = Uuid::parse_str(&execution_id.0)
            .map_err(|e| BackendError::Generic(format!("invalid execution_id: {e}")))?;
        let url = self.url(&format!("/api/execution-processes/{eid}"));
        loop {
            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .map_err(|e| BackendError::Generic(format!("GET {url}: {e}")))?;
            let proc: ExecProcView = Self::unwrap_envelope(resp, &format!("GET {url}")).await?;
            match proc.status.as_str() {
                "running" => tokio::time::sleep(self.poll_interval).await,
                "completed" => {
                    let last_assistant_message = self.fetch_last_assistant_message(&eid).await;
                    return Ok(ExecutionOutput {
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: proc.exit_code.map(|c| c as i32),
                        last_assistant_message,
                    });
                }
                other => {
                    return Err(BackendError::Generic(format!(
                        "execution {eid} ended with status `{other}`"
                    )));
                }
            }
        }
    }
}

impl VkApiBackend {
    async fn fetch_last_assistant_message(&self, execution_id: &Uuid) -> Option<String> {
        let url = self.url(&format!(
            "/api/execution-processes/{execution_id}/last-assistant-message"
        ));
        match self.client.get(&url).send().await {
            Ok(resp) => {
                Self::unwrap_envelope::<LastAssistantMessageView>(resp, &format!("GET {url}"))
                    .await
                    .ok()
                    .and_then(|v| v.message)
            }
            Err(err) => {
                tracing::warn!(%execution_id, %err, "failed to fetch last_assistant_message");
                None
            }
        }
    }
}

async fn gates_for_run(run_id: Uuid, pool: SqlitePool) -> GateEvaluators {
    let human: Box<dyn orchestration::gates::GateEvaluator> = if auto_approve_enabled() {
        tracing::info!(
            ?run_id,
            "VIBE_PROCEDURE_AUTO_APPROVE set; human gates will auto-approve"
        );
        Box::new(HumanGateEvaluator::new(AutoApprove(
            ApprovalResult::Approved,
        )))
    } else {
        let source = RegistryApproval::install(run_id, pool).await;
        Box::new(HumanGateEvaluator::new(source))
    };
    GateEvaluators {
        deterministic: Box::new(DeterministicGateEvaluator),
        llm_judge: Box::new(ContextLlmJudge),
        human,
    }
}

fn auto_approve_enabled() -> bool {
    matches!(
        std::env::var("VIBE_PROCEDURE_AUTO_APPROVE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    )
}

pub fn spawn_procedure_run(
    pool: SqlitePool,
    run_id: Uuid,
    procedure: Procedure,
    params: Value,
) -> JoinHandle<()> {
    let backend_kind = std::env::var("VIBE_PROCEDURE_BACKEND").unwrap_or_default();
    tokio::spawn(async move {
        let store = DbRunStore::new(pool.clone());
        let gates = gates_for_run(run_id, pool.clone()).await;
        let outcome = if backend_kind.eq_ignore_ascii_case("stub") {
            tracing::info!(?run_id, "procedure run using StubBackend");
            let executor = ProcedureExecutor::new(StubBackend, store, gates);
            executor
                .run_with_id(RunId(run_id), &procedure, params)
                .await
        } else {
            match VkApiBackend::from_env() {
                Ok(backend) => {
                    tracing::info!(?run_id, base_url = %backend.base_url, "procedure run using VkApiBackend");
                    let executor = ProcedureExecutor::new(backend, store, gates);
                    executor
                        .run_with_id(RunId(run_id), &procedure, params)
                        .await
                }
                Err(err) => {
                    tracing::error!(?run_id, %err, "failed to resolve VkApiBackend base URL; aborting run");
                    let _ = ProcedureRun::update_status(&pool, run_id, ProcedureRunStatus::Failed)
                        .await;
                    approvals().deregister(run_id).await;
                    return;
                }
            }
        };
        match outcome {
            Ok(outcome) => tracing::info!(?run_id, ?outcome, "procedure run completed"),
            Err(err) => {
                tracing::error!(?run_id, %err, "procedure run failed");
                let _ =
                    ProcedureRun::update_status(&pool, run_id, ProcedureRunStatus::Failed).await;
            }
        }
        approvals().deregister(run_id).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approval_registry_delivers_signal_to_registered_run() {
        let registry = ProcedureApprovalRegistry::new();
        let run_id = Uuid::new_v4();
        let (tx, mut rx) = mpsc::channel(1);
        registry.register(run_id, tx).await;

        let delivered = registry.signal(run_id, ApprovalResult::Approved).await;
        assert!(delivered);
        assert_eq!(rx.recv().await, Some(ApprovalResult::Approved));

        registry.deregister(run_id).await;
        let delivered_after = registry.signal(run_id, ApprovalResult::Rejected).await;
        assert!(!delivered_after);
    }

    #[tokio::test]
    async fn approval_registry_returns_false_for_unknown_run() {
        let registry = ProcedureApprovalRegistry::new();
        let delivered = registry
            .signal(Uuid::new_v4(), ApprovalResult::Approved)
            .await;
        assert!(!delivered);
    }

    #[test]
    fn converts_gate_pass_to_success_outcome() {
        let orch = OrchEntry {
            state: "review".into(),
            attempt: 1,
            event: StateEvent::GateEvaluated {
                passed: true,
                summary: "all checks green".into(),
            },
        };
        let db = convert_entry("review", "merge", &orch);
        assert_eq!(db.state, "review");
        assert_eq!(db.attempt, 1);
        assert_eq!(db.outcome, Some(StateOutcome::Success));
        assert_eq!(db.gate_summary.as_deref(), Some("all checks green"));
    }

    #[test]
    fn converts_gate_fail_to_failure_outcome() {
        let orch = OrchEntry {
            state: "test".into(),
            attempt: 2,
            event: StateEvent::GateEvaluated {
                passed: false,
                summary: "2 tests failed".into(),
            },
        };
        let db = convert_entry("test", "test", &orch);
        assert_eq!(db.outcome, Some(StateOutcome::Failure));
        assert_eq!(db.attempt, 2);
    }

    #[test]
    fn converts_retry_action_to_failure_outcome() {
        let orch = OrchEntry {
            state: "implement".into(),
            attempt: 1,
            event: StateEvent::ActionCompleted {
                action_kind: "follow_up".into(),
                session_id: Some("s1".into()),
                execution_id: Some("e1".into()),
            },
        };
        let db = convert_entry("implement", "implement", &orch);
        assert_eq!(db.outcome, Some(StateOutcome::Failure));
        assert_eq!(db.gate_summary.as_deref(), Some("action: follow_up"));
    }

    #[test]
    fn converts_progressing_action_to_success_outcome() {
        let orch = OrchEntry {
            state: "plan".into(),
            attempt: 1,
            event: StateEvent::ActionCompleted {
                action_kind: "create_session".into(),
                session_id: Some("s1".into()),
                execution_id: None,
            },
        };
        let db = convert_entry("plan", "implement", &orch);
        assert_eq!(db.outcome, Some(StateOutcome::Success));
    }

    #[test]
    fn converts_max_attempts_exhausted_to_failure() {
        let orch = OrchEntry {
            state: "test".into(),
            attempt: 3,
            event: StateEvent::MaxAttemptsExhausted { attempts: 3 },
        };
        let db = convert_entry("test", "failed", &orch);
        assert_eq!(db.outcome, Some(StateOutcome::Failure));
        assert!(db.gate_summary.as_deref().unwrap().contains("exhausted"));
    }

    mod vk_api_backend {
        use serde_json::json;
        use uuid::Uuid;
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{body_partial_json, method, path},
        };

        use super::*;

        fn envelope(data: serde_json::Value) -> serde_json::Value {
            json!({
                "success": true,
                "data": data,
                "error_data": null,
                "message": null
            })
        }

        #[tokio::test]
        async fn create_session_posts_session_then_initial_follow_up_and_awaits() {
            let server = MockServer::start().await;
            let session_uuid = Uuid::new_v4();
            let exec_uuid = Uuid::new_v4();
            let workspace_uuid = Uuid::new_v4();

            Mock::given(method("POST"))
                .and(path("/api/sessions"))
                .and(body_partial_json(json!({
                    "workspace_id": workspace_uuid,
                    "executor": "CLAUDE_CODE"
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": session_uuid,
                    "workspace_id": workspace_uuid,
                    "executor": "CLAUDE_CODE"
                }))))
                .expect(1)
                .mount(&server)
                .await;

            Mock::given(method("POST"))
                .and(path(format!("/api/sessions/{session_uuid}/follow-up")))
                .and(body_partial_json(json!({
                    "prompt": "do the thing",
                    "executor_config": { "executor": "CLAUDE_CODE" }
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": exec_uuid,
                    "status": "running",
                    "exit_code": null
                }))))
                .expect(1)
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path(format!("/api/execution-processes/{exec_uuid}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": exec_uuid,
                    "status": "running",
                    "exit_code": null
                }))))
                .up_to_n_times(1)
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path(format!("/api/execution-processes/{exec_uuid}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": exec_uuid,
                    "status": "completed",
                    "exit_code": 0
                }))))
                .mount(&server)
                .await;

            let backend =
                VkApiBackend::new(server.uri()).with_poll_interval(Duration::from_millis(10));
            let params = json!({ "workspace_id": workspace_uuid });
            let session_id = backend
                .create_session("CLAUDE_CODE", "do the thing", &params)
                .await
                .expect("create_session succeeds");
            assert_eq!(session_id.0, session_uuid.to_string());
        }

        #[tokio::test]
        async fn start_review_posts_and_returns_execution_id() {
            let server = MockServer::start().await;
            let session_uuid = Uuid::new_v4();
            let exec_uuid = Uuid::new_v4();

            Mock::given(method("POST"))
                .and(path(format!("/api/sessions/{session_uuid}/review")))
                .and(body_partial_json(json!({
                    "executor_config": { "executor": "CODEX" },
                    "additional_prompt": "review the diff",
                    "use_all_workspace_commits": false
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": exec_uuid,
                    "status": "running",
                    "exit_code": null
                }))))
                .expect(1)
                .mount(&server)
                .await;

            let backend = VkApiBackend::new(server.uri());
            let execution_id = backend
                .start_review(
                    SessionId(session_uuid.to_string()),
                    "CODEX",
                    "review the diff",
                )
                .await
                .expect("start_review succeeds");
            assert_eq!(execution_id.0, exec_uuid.to_string());
        }

        #[tokio::test]
        async fn await_completion_returns_error_on_failed_status() {
            let server = MockServer::start().await;
            let exec_uuid = Uuid::new_v4();

            Mock::given(method("GET"))
                .and(path(format!("/api/execution-processes/{exec_uuid}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": exec_uuid,
                    "status": "failed",
                    "exit_code": 1
                }))))
                .mount(&server)
                .await;

            let backend =
                VkApiBackend::new(server.uri()).with_poll_interval(Duration::from_millis(1));
            let err = backend
                .await_completion(ExecutionId(exec_uuid.to_string()))
                .await
                .expect_err("failed status should error");
            assert!(err.to_string().contains("failed"));
        }

        #[tokio::test]
        async fn await_completion_fetches_last_assistant_message_when_completed() {
            let server = MockServer::start().await;
            let exec_uuid = Uuid::new_v4();

            Mock::given(method("GET"))
                .and(path(format!("/api/execution-processes/{exec_uuid}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": exec_uuid,
                    "status": "completed",
                    "exit_code": 0
                }))))
                .expect(1)
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path(format!(
                    "/api/execution-processes/{exec_uuid}/last-assistant-message"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "message": "{\"verdict\":\"pass\"}"
                }))))
                .expect(1)
                .mount(&server)
                .await;

            let backend =
                VkApiBackend::new(server.uri()).with_poll_interval(Duration::from_millis(1));
            let output = backend
                .await_completion(ExecutionId(exec_uuid.to_string()))
                .await
                .expect("await_completion succeeds");
            assert_eq!(output.exit_code, Some(0));
            assert_eq!(
                output.last_assistant_message.as_deref(),
                Some("{\"verdict\":\"pass\"}")
            );
        }

        #[tokio::test]
        async fn await_completion_tolerates_missing_last_assistant_message() {
            let server = MockServer::start().await;
            let exec_uuid = Uuid::new_v4();

            Mock::given(method("GET"))
                .and(path(format!("/api/execution-processes/{exec_uuid}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": exec_uuid,
                    "status": "completed",
                    "exit_code": 0
                }))))
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path(format!(
                    "/api/execution-processes/{exec_uuid}/last-assistant-message"
                )))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;

            let backend =
                VkApiBackend::new(server.uri()).with_poll_interval(Duration::from_millis(1));
            let output = backend
                .await_completion(ExecutionId(exec_uuid.to_string()))
                .await
                .expect("await_completion succeeds even without summary");
            assert_eq!(output.last_assistant_message, None);
        }

        #[tokio::test]
        async fn merge_looks_up_workspace_repo_and_posts_merge() {
            let server = MockServer::start().await;
            let session_uuid = Uuid::new_v4();
            let workspace_uuid = Uuid::new_v4();
            let repo_uuid = Uuid::new_v4();

            Mock::given(method("GET"))
                .and(path(format!("/api/sessions/{session_uuid}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": session_uuid,
                    "workspace_id": workspace_uuid,
                    "executor": "CLAUDE_CODE"
                }))))
                .expect(1)
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path(format!("/api/workspaces/{workspace_uuid}/repos")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!([
                    { "repo": { "id": repo_uuid }, "target_branch": "main" }
                ]))))
                .expect(1)
                .mount(&server)
                .await;

            Mock::given(method("POST"))
                .and(path(format!("/api/workspaces/{workspace_uuid}/merge")))
                .and(body_partial_json(json!({ "repo_id": repo_uuid })))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!(null))))
                .expect(1)
                .mount(&server)
                .await;

            let backend = VkApiBackend::new(server.uri());
            let outcome = backend
                .merge(SessionId(session_uuid.to_string()))
                .await
                .expect("merge succeeds");
            match outcome {
                MergeOutcome::Merged { commit_sha } => {
                    assert!(commit_sha.contains(&workspace_uuid.to_string()));
                    assert!(commit_sha.contains(&repo_uuid.to_string()));
                }
                other => panic!("expected Merged, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn merge_errors_on_multi_repo_workspace() {
            let server = MockServer::start().await;
            let session_uuid = Uuid::new_v4();
            let workspace_uuid = Uuid::new_v4();

            Mock::given(method("GET"))
                .and(path(format!("/api/sessions/{session_uuid}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                    "id": session_uuid,
                    "workspace_id": workspace_uuid,
                    "executor": "CLAUDE_CODE"
                }))))
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path(format!("/api/workspaces/{workspace_uuid}/repos")))
                .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!([
                    { "repo": { "id": Uuid::new_v4() }, "target_branch": "main" },
                    { "repo": { "id": Uuid::new_v4() }, "target_branch": "main" }
                ]))))
                .mount(&server)
                .await;

            let backend = VkApiBackend::new(server.uri());
            let err = backend
                .merge(SessionId(session_uuid.to_string()))
                .await
                .expect_err("multi-repo workspace should error");
            assert!(err.to_string().contains("exactly one"), "got: {err}");
        }
    }
}
