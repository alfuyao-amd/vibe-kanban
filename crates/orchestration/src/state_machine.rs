//! Procedure runtime.
//!
//! Template interpolation supports `{{name}}` and `{{path.to.field}}` over the
//! current run context: initial trigger params at the root, plus nested
//! objects for each previously-completed gate (`gate.output`, `gate.response`,
//! and `gate.<state>.output` / `gate.<state>.response`). Unknown placeholders
//! are left unchanged.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    gates::{GateContext, GateError, GateEvaluator, GateOutcome},
    procedure::{Action, Gate, Procedure, Terminal, ValidationError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RunId(pub Uuid);

impl RunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExecutionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged { commit_sha: String },
    Conflict { detail: String },
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("backend failure: {0}")]
    Generic(String),
}

#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    async fn create_session(
        &self,
        executor: &str,
        prompt: &str,
        params: &Value,
    ) -> Result<SessionId, BackendError>;

    async fn follow_up(
        &self,
        session_id: SessionId,
        executor: Option<&str>,
        prompt: &str,
    ) -> Result<ExecutionId, BackendError>;

    async fn start_review(
        &self,
        session_id: SessionId,
        executor: &str,
        prompt: &str,
    ) -> Result<ExecutionId, BackendError>;

    async fn merge(&self, session_id: SessionId) -> Result<MergeOutcome, BackendError>;

    async fn await_completion(
        &self,
        execution_id: ExecutionId,
    ) -> Result<ExecutionOutput, BackendError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateEvent {
    ActionCompleted {
        action_kind: String,
        session_id: Option<String>,
        execution_id: Option<String>,
    },
    GateEvaluated {
        passed: bool,
        summary: String,
    },
    MaxAttemptsExhausted {
        attempts: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateHistoryEntry {
    pub state: String,
    pub attempt: u32,
    pub event: StateEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Success,
    Failure,
    /// Run was interrupted by a cancellation signal. State machine bails between
    /// states (and from any in-flight backend await it can drop), so observed
    /// latency is bounded by the longest backend operation in flight.
    Cancelled,
}

#[async_trait]
pub trait RunStore: Send + Sync {
    async fn persist_transition(
        &self,
        run_id: RunId,
        from_state: &str,
        to_state: &str,
        entry: StateHistoryEntry,
    );

    async fn set_terminal(&self, run_id: RunId, outcome: RunOutcome);
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryRunStore {
    inner: Arc<Mutex<InMemoryRunStoreInner>>,
}

#[derive(Debug, Default)]
struct InMemoryRunStoreInner {
    transitions: HashMap<RunId, Vec<(String, String, StateHistoryEntry)>>,
    terminals: HashMap<RunId, RunOutcome>,
}

impl InMemoryRunStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn transitions(&self, run_id: RunId) -> Vec<(String, String, StateHistoryEntry)> {
        let guard = self.inner.lock().expect("run store mutex poisoned");
        guard.transitions.get(&run_id).cloned().unwrap_or_default()
    }

    pub fn terminal(&self, run_id: RunId) -> Option<RunOutcome> {
        let guard = self.inner.lock().expect("run store mutex poisoned");
        guard.terminals.get(&run_id).cloned()
    }
}

#[async_trait]
impl RunStore for InMemoryRunStore {
    async fn persist_transition(
        &self,
        run_id: RunId,
        from_state: &str,
        to_state: &str,
        entry: StateHistoryEntry,
    ) {
        let mut guard = self.inner.lock().expect("run store mutex poisoned");
        guard.transitions.entry(run_id).or_default().push((
            from_state.to_string(),
            to_state.to_string(),
            entry,
        ));
    }

    async fn set_terminal(&self, run_id: RunId, outcome: RunOutcome) {
        let mut guard = self.inner.lock().expect("run store mutex poisoned");
        guard.terminals.insert(run_id, outcome);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("procedure validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("unknown state `{0}`")]
    UnknownState(String),
    #[error("state `{0}` has no transition defined for outcome `{1}`")]
    MissingTransition(String, &'static str),
    #[error("state `{state}` exhausted max_attempts ({attempts}) with no on_failure")]
    MaxAttemptsExhausted { state: String, attempts: u32 },
    #[error("session_ref `{0}` not found in run context")]
    UnknownSessionRef(String),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error(transparent)]
    Gate(#[from] GateError),
}

pub struct GateEvaluators {
    pub deterministic: Box<dyn GateEvaluator>,
    pub llm_judge: Box<dyn GateEvaluator>,
    pub human: Box<dyn GateEvaluator>,
}

pub struct ProcedureExecutor<B: ExecutionBackend, S: RunStore> {
    backend: B,
    store: S,
    gates: GateEvaluators,
}

/// Outcome of a single state's action+gate cycle.
#[derive(Debug)]
enum StateOutput {
    Completed { passed: bool, event: StateEvent },
    Cancelled,
}

impl<B: ExecutionBackend, S: RunStore> ProcedureExecutor<B, S> {
    pub fn new(backend: B, store: S, gates: GateEvaluators) -> Self {
        Self {
            backend,
            store,
            gates,
        }
    }

    pub async fn run(
        &self,
        procedure: &Procedure,
        initial_params: Value,
    ) -> Result<RunOutcome, ExecutorError> {
        self.run_with_id(RunId::new(), procedure, initial_params)
            .await
    }

    pub async fn run_with_id(
        &self,
        run_id: RunId,
        procedure: &Procedure,
        initial_params: Value,
    ) -> Result<RunOutcome, ExecutorError> {
        self.run_with_id_cancellable(run_id, procedure, initial_params, None)
            .await
    }

    /// Like [`run_with_id`] but observes `cancel_token`. When cancellation is
    /// requested, the executor short-circuits to `RunOutcome::Cancelled`,
    /// drops any in-flight backend future, and writes the terminal status to
    /// the store before returning. Pass `None` to opt out (equivalent to
    /// [`run_with_id`]).
    pub async fn run_with_id_cancellable(
        &self,
        run_id: RunId,
        procedure: &Procedure,
        initial_params: Value,
        cancel_token: Option<CancellationToken>,
    ) -> Result<RunOutcome, ExecutorError> {
        procedure.validate()?;
        let mut context = RunContext::new(initial_params);
        let mut current = procedure.initial_state.clone();
        let mut attempts: HashMap<String, u32> = HashMap::new();

        loop {
            // Cancellation check at the top of every iteration. This catches
            // cancellation issued between states even when no backend await
            // is in flight.
            if Self::is_cancelled(&cancel_token) {
                self.store.set_terminal(run_id, RunOutcome::Cancelled).await;
                return Ok(RunOutcome::Cancelled);
            }

            let state = procedure
                .states
                .get(&current)
                .ok_or_else(|| ExecutorError::UnknownState(current.clone()))?;

            if let Some(terminal) = state.terminal {
                let outcome = match terminal {
                    Terminal::Success => RunOutcome::Success,
                    Terminal::Failure => RunOutcome::Failure,
                };
                self.store.set_terminal(run_id, outcome.clone()).await;
                return Ok(outcome);
            }

            let attempt = {
                let entry = attempts.entry(current.clone()).or_insert(0);
                *entry += 1;
                *entry
            };

            let (passed, history_event) = match self
                .execute_state_cancellable(&current, state, &mut context, &cancel_token)
                .await?
            {
                StateOutput::Completed { passed, event } => (passed, event),
                StateOutput::Cancelled => {
                    self.store.set_terminal(run_id, RunOutcome::Cancelled).await;
                    return Ok(RunOutcome::Cancelled);
                }
            };

            let max_attempts = state.max_attempts.unwrap_or(1);
            let effective_passed = if passed {
                true
            } else if attempt < max_attempts {
                let entry = StateHistoryEntry {
                    state: current.clone(),
                    attempt,
                    event: history_event.clone(),
                };
                self.store
                    .persist_transition(run_id, &current, &current, entry)
                    .await;
                continue;
            } else {
                if attempt > 1 {
                    let exhaust_entry = StateHistoryEntry {
                        state: current.clone(),
                        attempt,
                        event: StateEvent::MaxAttemptsExhausted { attempts: attempt },
                    };
                    self.store
                        .persist_transition(run_id, &current, &current, exhaust_entry)
                        .await;
                }
                false
            };

            let next = if effective_passed {
                state.on_success.clone().ok_or_else(|| {
                    ExecutorError::MissingTransition(current.clone(), "on_success")
                })?
            } else {
                match state.on_failure.clone() {
                    Some(t) => t,
                    None => {
                        return Err(ExecutorError::MaxAttemptsExhausted {
                            state: current.clone(),
                            attempts: attempt,
                        });
                    }
                }
            };

            if !procedure.states.contains_key(&next) {
                return Err(ExecutorError::UnknownState(next));
            }

            let entry = StateHistoryEntry {
                state: current.clone(),
                attempt,
                event: history_event,
            };
            self.store
                .persist_transition(run_id, &current, &next, entry)
                .await;
            current = next;
        }
    }

    fn is_cancelled(token: &Option<CancellationToken>) -> bool {
        token.as_ref().is_some_and(|t| t.is_cancelled())
    }

    async fn execute_state_cancellable(
        &self,
        state_name: &str,
        state: &crate::procedure::State,
        context: &mut RunContext,
        cancel_token: &Option<CancellationToken>,
    ) -> Result<StateOutput, ExecutorError> {
        let work = self.execute_state(state_name, state, context);

        if let Some(token) = cancel_token {
            tokio::select! {
                biased;
                _ = token.cancelled() => Ok(StateOutput::Cancelled),
                res = work => res.map(|(passed, event)| StateOutput::Completed { passed, event }),
            }
        } else {
            let (passed, event) = work.await?;
            Ok(StateOutput::Completed { passed, event })
        }
    }

    async fn execute_state(
        &self,
        state_name: &str,
        state: &crate::procedure::State,
        context: &mut RunContext,
    ) -> Result<(bool, StateEvent), ExecutorError> {
        let mut action_event: Option<StateEvent> = None;

        if let Some(action) = &state.action {
            let rendered = render_action(action, context);
            match rendered {
                RenderedAction::CreateSession { executor, prompt } => {
                    let session = self
                        .backend
                        .create_session(&executor, &prompt, &context.initial)
                        .await?;
                    context
                        .sessions
                        .insert(state_name.to_string(), session.clone());
                    action_event = Some(StateEvent::ActionCompleted {
                        action_kind: "create_session".to_string(),
                        session_id: Some(session.0),
                        execution_id: None,
                    });
                }
                RenderedAction::FollowUp {
                    session_ref,
                    executor,
                    prompt,
                } => {
                    let session = context
                        .sessions
                        .get(&session_ref)
                        .cloned()
                        .ok_or(ExecutorError::UnknownSessionRef(session_ref))?;
                    let execution = self
                        .backend
                        .follow_up(session.clone(), executor.as_deref(), &prompt)
                        .await?;
                    let output = self.backend.await_completion(execution.clone()).await?;
                    context.last_action_output = Some(output);
                    action_event = Some(StateEvent::ActionCompleted {
                        action_kind: "follow_up".to_string(),
                        session_id: Some(session.0),
                        execution_id: Some(execution.0),
                    });
                }
                RenderedAction::StartReview {
                    session_ref,
                    executor,
                    prompt,
                } => {
                    let session = context
                        .sessions
                        .get(&session_ref)
                        .cloned()
                        .ok_or(ExecutorError::UnknownSessionRef(session_ref))?;
                    let execution = self
                        .backend
                        .start_review(session.clone(), &executor, &prompt)
                        .await?;
                    let output = self.backend.await_completion(execution.clone()).await?;
                    context.last_action_output = Some(output);
                    action_event = Some(StateEvent::ActionCompleted {
                        action_kind: "start_review".to_string(),
                        session_id: Some(session.0),
                        execution_id: Some(execution.0),
                    });
                }
                RenderedAction::Merge { session_ref } => {
                    let session = context
                        .sessions
                        .get(&session_ref)
                        .cloned()
                        .ok_or(ExecutorError::UnknownSessionRef(session_ref))?;
                    let outcome = self.backend.merge(session.clone()).await?;
                    let merged = matches!(outcome, MergeOutcome::Merged { .. });
                    action_event = Some(StateEvent::ActionCompleted {
                        action_kind: "merge".to_string(),
                        session_id: Some(session.0),
                        execution_id: None,
                    });
                    if !merged {
                        return Ok((false, action_event.unwrap()));
                    }
                }
            }
        }

        if let Some(gate) = &state.gate {
            let outcome = self.evaluate_gate(gate, context).await?;
            context.update_gate(state_name, &outcome);
            let event = StateEvent::GateEvaluated {
                passed: outcome.passed,
                summary: outcome.summary.clone(),
            };
            return Ok((outcome.passed, event));
        }

        let event = action_event.unwrap_or(StateEvent::GateEvaluated {
            passed: true,
            summary: "no-op".to_string(),
        });
        Ok((true, event))
    }

    async fn evaluate_gate(
        &self,
        gate: &Gate,
        context: &RunContext,
    ) -> Result<GateOutcome, ExecutorError> {
        let last_assistant_message = context
            .last_action_output
            .as_ref()
            .and_then(|o| o.last_assistant_message.as_deref());
        match gate {
            Gate::Deterministic { run, pass_when } => {
                let run_rendered = render_template(run, &context.view());
                let ctx = GateContext {
                    run: Some(run_rendered),
                    pass_when,
                    prompt: None,
                    last_assistant_message,
                };
                Ok(self.gates.deterministic.evaluate(&ctx).await?)
            }
            Gate::LlmJudge { pass_when } => {
                let ctx = GateContext {
                    run: None,
                    pass_when,
                    prompt: None,
                    last_assistant_message,
                };
                Ok(self.gates.llm_judge.evaluate(&ctx).await?)
            }
            Gate::Human { prompt } => {
                let prompt_rendered = render_template(prompt, &context.view());
                let ctx = GateContext {
                    run: None,
                    pass_when: "",
                    prompt: Some(prompt_rendered),
                    last_assistant_message,
                };
                Ok(self.gates.human.evaluate(&ctx).await?)
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RunContext {
    initial: Value,
    sessions: HashMap<String, SessionId>,
    last_action_output: Option<ExecutionOutput>,
    last_gate: Option<GateSnapshot>,
    gates_by_state: HashMap<String, GateSnapshot>,
}

#[derive(Debug, Clone)]
struct GateSnapshot {
    output: Option<String>,
    response: Option<Value>,
}

impl RunContext {
    fn new(initial: Value) -> Self {
        Self {
            initial,
            sessions: HashMap::new(),
            last_action_output: None,
            last_gate: None,
            gates_by_state: HashMap::new(),
        }
    }

    fn update_gate(&mut self, state_name: &str, outcome: &GateOutcome) {
        let snap = GateSnapshot {
            output: outcome.output.clone(),
            response: outcome.response.clone(),
        };
        self.gates_by_state
            .insert(state_name.to_string(), snap.clone());
        self.last_gate = Some(snap);
    }

    fn view(&self) -> Value {
        let mut root = match self.initial.clone() {
            Value::Object(map) => Value::Object(map),
            other => json!({ "value": other }),
        };
        if let Some(last) = &self.last_gate {
            let mut gate_obj = serde_json::Map::new();
            if let Some(output) = &last.output {
                gate_obj.insert("output".to_string(), Value::String(output.clone()));
            }
            if let Some(response) = &last.response {
                gate_obj.insert("response".to_string(), response.clone());
            }
            for (name, snap) in &self.gates_by_state {
                let mut sub = serde_json::Map::new();
                if let Some(output) = &snap.output {
                    sub.insert("output".to_string(), Value::String(output.clone()));
                }
                if let Some(response) = &snap.response {
                    sub.insert("response".to_string(), response.clone());
                }
                gate_obj.insert(name.clone(), Value::Object(sub));
            }
            if let Value::Object(map) = &mut root {
                map.insert("gate".to_string(), Value::Object(gate_obj));
            }
        }
        root
    }
}

enum RenderedAction {
    CreateSession {
        executor: String,
        prompt: String,
    },
    FollowUp {
        session_ref: String,
        executor: Option<String>,
        prompt: String,
    },
    StartReview {
        session_ref: String,
        executor: String,
        prompt: String,
    },
    Merge {
        session_ref: String,
    },
}

fn render_action(action: &Action, context: &RunContext) -> RenderedAction {
    let view = context.view();
    match action {
        Action::CreateSession { executor, prompt } => RenderedAction::CreateSession {
            executor: executor.clone(),
            prompt: render_template(prompt, &view),
        },
        Action::FollowUp {
            session_ref,
            executor,
            prompt,
        } => RenderedAction::FollowUp {
            session_ref: session_ref.clone(),
            executor: executor.clone(),
            prompt: render_template(prompt, &view),
        },
        Action::StartReview {
            session_ref,
            executor,
            prompt,
        } => RenderedAction::StartReview {
            session_ref: session_ref.clone(),
            executor: executor.clone(),
            prompt: render_template(prompt, &view),
        },
        Action::Merge { session_ref } => RenderedAction::Merge {
            session_ref: session_ref.clone(),
        },
    }
}

/// Render `{{ path | filter1 | filter2(arg) }}` against `context`.
///
/// The lookup is `path.to.field` over the JSON view (same as before).
/// Supported filters (all stringify the result):
/// - `default("x")` — fall back to the literal string `x` when the looked-up
///   value is missing or null. Without a default, an unresolved placeholder
///   is left in the template verbatim (back-compat).
/// - `upper` / `lower` — ASCII case transforms.
/// - `shell_quote` — POSIX-shell-quote the value (single-quotes, with `'\''`
///   escapes). Use to interpolate into shell commands safely:
///   `cd {{ workspace.worktree_path | shell_quote }} && {{ test_command }}`.
/// - `json` — JSON-encode the value (handy for embedding into prompts).
///
/// Filters apply left-to-right. Unknown filters leave the placeholder
/// untouched (with the verbatim template segment in the output) so a typo
/// is visible rather than silent.
pub fn render_template(template: &str, context: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len()
            && bytes[i] == b'{'
            && bytes[i + 1] == b'{'
            && let Some(end) = find_close(bytes, i + 2)
        {
            let raw = &template[i + 2..end];
            match render_placeholder(raw, context) {
                Some(rendered) => out.push_str(&rendered),
                None => out.push_str(&template[i..end + 2]),
            }
            i = end + 2;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Resolve a single `{{ ... }}` body. Returns `None` to leave the placeholder
/// verbatim in the output (unknown var without `default`, unknown filter, or
/// malformed filter args).
fn render_placeholder(raw: &str, context: &Value) -> Option<String> {
    let mut parts = raw.split('|');
    let path = parts.next()?.trim();
    let filters: Vec<&str> = parts.map(str::trim).collect();

    let mut current = lookup_path(context, path);

    for filter in &filters {
        let (name, args) = parse_filter(filter)?;
        match name {
            "default" => {
                let fallback = args.first()?.clone();
                if current.as_ref().is_none_or(Value::is_null) {
                    current = Some(Value::String(fallback));
                }
            }
            "upper" => {
                let s = current.as_ref().map(stringify).unwrap_or_default();
                current = Some(Value::String(s.to_uppercase()));
            }
            "lower" => {
                let s = current.as_ref().map(stringify).unwrap_or_default();
                current = Some(Value::String(s.to_lowercase()));
            }
            "shell_quote" => {
                let s = current.as_ref().map(stringify).unwrap_or_default();
                current = Some(Value::String(shell_quote(&s)));
            }
            "json" => {
                let serialized = current
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()))
                    .unwrap_or_else(|| "null".to_string());
                current = Some(Value::String(serialized));
            }
            _ => return None,
        }
    }

    current.map(|v| stringify(&v))
}

/// Parse a filter chunk like `default("x")`, `default('x')`, or `upper` into
/// `(name, [args...])`. Returns `None` on malformed input so the renderer
/// can leave the placeholder verbatim — typos shouldn't render as empty.
fn parse_filter(raw: &str) -> Option<(&str, Vec<String>)> {
    let raw = raw.trim();
    let Some(open) = raw.find('(') else {
        return Some((raw, Vec::new()));
    };
    if !raw.ends_with(')') {
        return None;
    }
    let name = raw[..open].trim();
    let inner = &raw[open + 1..raw.len() - 1];
    let arg = parse_filter_arg(inner.trim())?;
    Some((name, vec![arg]))
}

fn parse_filter_arg(raw: &str) -> Option<String> {
    if raw.len() >= 2
        && let Some(stripped) = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
    {
        return Some(stripped.to_string());
    }
    None
}

/// POSIX-style single-quote shell escape: `it's` → `'it'\''s'`. Empty input
/// becomes `''` (a valid empty argument).
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn find_close(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn lookup_path(value: &Value, path: &str) -> Option<Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current.clone())
}

fn stringify(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use indexmap::IndexMap;
    use serde_json::json;

    use super::*;
    use crate::{
        gates::{
            ApprovalResult, AutoApprove, DeterministicGateEvaluator, HumanGateEvaluator,
            LlmJudgeGateEvaluator, StaticLlmJudgeSource,
        },
        procedure::{Action, Gate, State, Terminal},
    };

    fn default_gates() -> GateEvaluators {
        GateEvaluators {
            deterministic: Box::new(DeterministicGateEvaluator),
            llm_judge: Box::new(LlmJudgeGateEvaluator::new(StaticLlmJudgeSource(json!({
                "verdict": "pass"
            })))),
            human: Box::new(HumanGateEvaluator::new(AutoApprove(
                ApprovalResult::Approved,
            ))),
        }
    }

    #[derive(Default)]
    struct MockBackend {
        create_calls: AtomicUsize,
        follow_up_calls: AtomicUsize,
        review_calls: AtomicUsize,
        merge_calls: AtomicUsize,
    }

    #[async_trait]
    impl ExecutionBackend for MockBackend {
        async fn create_session(
            &self,
            _executor: &str,
            _prompt: &str,
            _params: &Value,
        ) -> Result<SessionId, BackendError> {
            let n = self.create_calls.fetch_add(1, Ordering::SeqCst);
            Ok(SessionId(format!("session-{n}")))
        }

        async fn follow_up(
            &self,
            _session_id: SessionId,
            _executor: Option<&str>,
            _prompt: &str,
        ) -> Result<ExecutionId, BackendError> {
            let n = self.follow_up_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ExecutionId(format!("exec-fu-{n}")))
        }

        async fn start_review(
            &self,
            _session_id: SessionId,
            _executor: &str,
            _prompt: &str,
        ) -> Result<ExecutionId, BackendError> {
            let n = self.review_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ExecutionId(format!("exec-rv-{n}")))
        }

        async fn merge(&self, _session_id: SessionId) -> Result<MergeOutcome, BackendError> {
            self.merge_calls.fetch_add(1, Ordering::SeqCst);
            Ok(MergeOutcome::Merged {
                commit_sha: "abc".to_string(),
            })
        }

        async fn await_completion(
            &self,
            _execution_id: ExecutionId,
        ) -> Result<ExecutionOutput, BackendError> {
            Ok(ExecutionOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
                last_assistant_message: None,
            })
        }
    }

    fn two_state_procedure() -> Procedure {
        let mut states = IndexMap::new();
        states.insert(
            "plan".to_string(),
            State {
                action: Some(Action::CreateSession {
                    executor: "claude".to_string(),
                    prompt: "plan".to_string(),
                }),
                on_success: Some("done".to_string()),
                on_failure: Some("failed".to_string()),
                ..Default::default()
            },
        );
        states.insert(
            "done".to_string(),
            State {
                terminal: Some(Terminal::Success),
                ..Default::default()
            },
        );
        states.insert(
            "failed".to_string(),
            State {
                terminal: Some(Terminal::Failure),
                ..Default::default()
            },
        );
        Procedure {
            name: "test".to_string(),
            version: 1,
            description: String::new(),
            triggers: Default::default(),
            initial_state: "plan".to_string(),
            states,
        }
    }

    #[tokio::test]
    async fn happy_path_reaches_terminal_success() {
        let store = InMemoryRunStore::new();
        let executor =
            ProcedureExecutor::new(MockBackend::default(), store.clone(), default_gates());
        let outcome = executor
            .run(&two_state_procedure(), json!({}))
            .await
            .unwrap();
        assert_eq!(outcome, RunOutcome::Success);
    }

    #[tokio::test]
    async fn terminal_state_stops_execution() {
        let mut proc = two_state_procedure();
        proc.initial_state = "done".to_string();
        let store = InMemoryRunStore::new();
        let executor = ProcedureExecutor::new(MockBackend::default(), store, default_gates());
        let outcome = executor.run(&proc, json!({})).await.unwrap();
        assert_eq!(outcome, RunOutcome::Success);
    }

    #[tokio::test]
    async fn unknown_transition_returns_error() {
        let mut states = IndexMap::new();
        states.insert(
            "plan".to_string(),
            State {
                action: Some(Action::CreateSession {
                    executor: "claude".to_string(),
                    prompt: "hi".to_string(),
                }),
                on_success: Some("ghost".to_string()),
                ..Default::default()
            },
        );
        let proc = Procedure {
            name: "bad".to_string(),
            version: 1,
            description: String::new(),
            triggers: Default::default(),
            initial_state: "plan".to_string(),
            states,
        };
        let store = InMemoryRunStore::new();
        let executor = ProcedureExecutor::new(MockBackend::default(), store, default_gates());
        let err = executor.run(&proc, json!({})).await.unwrap_err();
        assert!(matches!(err, ExecutorError::Validation(_)));
    }

    #[tokio::test]
    async fn max_attempts_exhaustion_falls_through_to_on_failure() {
        let mut states = IndexMap::new();
        states.insert(
            "start".to_string(),
            State {
                action: Some(Action::CreateSession {
                    executor: "claude".to_string(),
                    prompt: "go".to_string(),
                }),
                gate: Some(Gate::Deterministic {
                    run: "exit 1".to_string(),
                    pass_when: "exit_code == 0".to_string(),
                }),
                max_attempts: Some(2),
                on_success: Some("done".to_string()),
                on_failure: Some("failed".to_string()),
                ..Default::default()
            },
        );
        states.insert(
            "done".to_string(),
            State {
                terminal: Some(Terminal::Success),
                ..Default::default()
            },
        );
        states.insert(
            "failed".to_string(),
            State {
                terminal: Some(Terminal::Failure),
                ..Default::default()
            },
        );
        let proc = Procedure {
            name: "retry".to_string(),
            version: 1,
            description: String::new(),
            triggers: Default::default(),
            initial_state: "start".to_string(),
            states,
        };
        let store = InMemoryRunStore::new();
        let backend = MockBackend::default();
        let executor = ProcedureExecutor::new(backend, store.clone(), default_gates());
        let outcome = executor.run(&proc, json!({})).await.unwrap();
        assert_eq!(outcome, RunOutcome::Failure);
        assert_eq!(executor.backend.create_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn missing_on_failure_after_exhaustion_errors() {
        let mut states = IndexMap::new();
        states.insert(
            "start".to_string(),
            State {
                action: Some(Action::CreateSession {
                    executor: "claude".to_string(),
                    prompt: "go".to_string(),
                }),
                gate: Some(Gate::Deterministic {
                    run: "exit 1".to_string(),
                    pass_when: "exit_code == 0".to_string(),
                }),
                max_attempts: Some(1),
                on_success: Some("done".to_string()),
                on_failure: None,
                ..Default::default()
            },
        );
        states.insert(
            "done".to_string(),
            State {
                terminal: Some(Terminal::Success),
                ..Default::default()
            },
        );
        let proc = Procedure {
            name: "err".to_string(),
            version: 1,
            description: String::new(),
            triggers: Default::default(),
            initial_state: "start".to_string(),
            states,
        };
        let store = InMemoryRunStore::new();
        let executor = ProcedureExecutor::new(MockBackend::default(), store, default_gates());
        let err = executor.run(&proc, json!({})).await.unwrap_err();
        assert!(matches!(err, ExecutorError::MaxAttemptsExhausted { .. }));
    }

    #[tokio::test]
    async fn feature_with_tests_reaches_success_under_stub_runtime() {
        use crate::{
            gates::{ContextLlmJudge, DeterministicGateEvaluator},
            loader::builtin_procedures,
        };

        #[derive(Default)]
        struct StubBackend;

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
                    commit_sha: "stub".into(),
                })
            }
            async fn await_completion(
                &self,
                _execution_id: ExecutionId,
            ) -> Result<ExecutionOutput, BackendError> {
                Ok(ExecutionOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    last_assistant_message: Some(
                        r#"{"verdict":"pass","feedback":"ok"}"#.to_string(),
                    ),
                })
            }
        }

        let procs = builtin_procedures().expect("builtins parse");
        let feature = procs
            .into_iter()
            .find(|p| p.name == "feature_with_tests")
            .expect("feature_with_tests exists");
        let gates = GateEvaluators {
            deterministic: Box::new(DeterministicGateEvaluator),
            llm_judge: Box::new(ContextLlmJudge),
            human: Box::new(crate::gates::HumanGateEvaluator::new(
                crate::gates::AutoApprove(crate::gates::ApprovalResult::Approved),
            )),
        };
        let executor = ProcedureExecutor::new(StubBackend, InMemoryRunStore::new(), gates);
        let outcome = executor
            .run(
                &feature,
                json!({
                    "goal": "add shout(text)",
                    "test_command": "true",
                    "workspace": { "worktree_path": "/tmp" },
                }),
            )
            .await
            .unwrap();
        assert_eq!(outcome, RunOutcome::Success);
    }

    #[tokio::test]
    async fn llm_judge_consumes_reviewer_output_and_feedback_flows_to_next_state() {
        use std::sync::Mutex;

        use crate::gates::ContextLlmJudge;

        #[derive(Default)]
        struct ScriptedBackend {
            follow_up_prompts: Mutex<Vec<String>>,
        }

        #[async_trait]
        impl ExecutionBackend for ScriptedBackend {
            async fn create_session(
                &self,
                _executor: &str,
                _prompt: &str,
                _params: &Value,
            ) -> Result<SessionId, BackendError> {
                Ok(SessionId("s1".into()))
            }
            async fn follow_up(
                &self,
                _session_id: SessionId,
                _executor: Option<&str>,
                prompt: &str,
            ) -> Result<ExecutionId, BackendError> {
                self.follow_up_prompts.lock().unwrap().push(prompt.into());
                Ok(ExecutionId("e-fu".into()))
            }
            async fn start_review(
                &self,
                _session_id: SessionId,
                _executor: &str,
                _prompt: &str,
            ) -> Result<ExecutionId, BackendError> {
                Ok(ExecutionId("e-rv".into()))
            }
            async fn merge(&self, _session_id: SessionId) -> Result<MergeOutcome, BackendError> {
                Ok(MergeOutcome::Merged {
                    commit_sha: "abc".into(),
                })
            }
            async fn await_completion(
                &self,
                execution_id: ExecutionId,
            ) -> Result<ExecutionOutput, BackendError> {
                let msg = match execution_id.0.as_str() {
                    "e-rv" => Some(r#"{"verdict": "fail", "feedback": "needs tests"}"#.into()),
                    _ => None,
                };
                Ok(ExecutionOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    last_assistant_message: msg,
                })
            }
        }

        let mut states = IndexMap::new();
        states.insert(
            "plan".into(),
            State {
                action: Some(Action::CreateSession {
                    executor: "c".into(),
                    prompt: "p".into(),
                }),
                on_success: Some("review".into()),
                ..Default::default()
            },
        );
        states.insert(
            "review".into(),
            State {
                action: Some(Action::StartReview {
                    session_ref: "plan".into(),
                    executor: "c".into(),
                    prompt: "review".into(),
                }),
                gate: Some(Gate::LlmJudge {
                    pass_when: "response.verdict == 'pass'".into(),
                }),
                on_success: Some("merge".into()),
                on_failure: Some("address".into()),
                ..Default::default()
            },
        );
        states.insert(
            "address".into(),
            State {
                action: Some(Action::FollowUp {
                    session_ref: "plan".into(),
                    executor: None,
                    prompt: "fix: {{gate.response.feedback}}".into(),
                }),
                on_success: Some("done".into()),
                ..Default::default()
            },
        );
        states.insert(
            "merge".into(),
            State {
                terminal: Some(Terminal::Success),
                ..Default::default()
            },
        );
        states.insert(
            "done".into(),
            State {
                terminal: Some(Terminal::Success),
                ..Default::default()
            },
        );
        let proc = Procedure {
            name: "t".into(),
            version: 1,
            description: String::new(),
            triggers: Default::default(),
            initial_state: "plan".into(),
            states,
        };
        let gates = GateEvaluators {
            deterministic: Box::new(DeterministicGateEvaluator),
            llm_judge: Box::new(ContextLlmJudge),
            human: Box::new(HumanGateEvaluator::new(AutoApprove(
                ApprovalResult::Approved,
            ))),
        };
        let backend = ScriptedBackend::default();
        let executor = ProcedureExecutor::new(backend, InMemoryRunStore::new(), gates);
        let outcome = executor.run(&proc, json!({})).await.unwrap();
        assert_eq!(outcome, RunOutcome::Success);
        let prompts = executor.backend.follow_up_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0], "fix: needs tests");
    }

    #[test]
    fn template_replaces_simple_placeholder() {
        let ctx = json!({"foo": "bar"});
        assert_eq!(render_template("hello {{foo}}", &ctx), "hello bar");
    }

    #[test]
    fn template_walks_nested_path() {
        let ctx = json!({"foo": {"bar": "baz"}});
        assert_eq!(render_template("{{foo.bar}}", &ctx), "baz");
    }

    #[test]
    fn template_leaves_unknown_placeholders_unchanged() {
        let ctx = json!({"foo": "bar"});
        assert_eq!(
            render_template("{{missing}} {{foo}}", &ctx),
            "{{missing}} bar"
        );
    }

    #[test]
    fn template_handles_integer_values() {
        let ctx = json!({"n": 42});
        assert_eq!(render_template("x={{n}}", &ctx), "x=42");
    }

    #[test]
    fn template_default_filter_fills_missing_var() {
        let ctx = json!({});
        assert_eq!(
            render_template(r#"{{ name | default("anon") }}"#, &ctx),
            "anon"
        );
    }

    #[test]
    fn template_default_filter_skipped_when_var_present() {
        let ctx = json!({"name": "ada"});
        assert_eq!(
            render_template(r#"{{ name | default("anon") }}"#, &ctx),
            "ada"
        );
    }

    #[test]
    fn template_default_filter_treats_explicit_null_as_absent() {
        let ctx = json!({"name": null});
        assert_eq!(
            render_template(r#"{{ name | default("anon") }}"#, &ctx),
            "anon"
        );
    }

    #[test]
    fn template_upper_lower_filters() {
        let ctx = json!({"name": "Ada"});
        assert_eq!(render_template("{{name | upper}}", &ctx), "ADA");
        assert_eq!(render_template("{{name | lower}}", &ctx), "ada");
    }

    #[test]
    fn template_shell_quote_escapes_single_quotes() {
        let ctx = json!({"path": "/tmp/it's a path/file.txt"});
        let out = render_template("cd {{ path | shell_quote }}", &ctx);
        // Echo into bash and you get `/tmp/it's a path/file.txt` back.
        assert_eq!(out, r#"cd '/tmp/it'\''s a path/file.txt'"#);
    }

    #[test]
    fn template_shell_quote_handles_empty() {
        let ctx = json!({"empty": ""});
        assert_eq!(render_template("x={{empty | shell_quote}}", &ctx), "x=''");
    }

    #[test]
    fn template_json_filter_serializes_object() {
        let ctx = json!({"obj": {"a": 1}});
        assert_eq!(render_template("{{obj | json}}", &ctx), r#"{"a":1}"#);
    }

    #[test]
    fn template_filters_chain_left_to_right() {
        let ctx = json!({});
        assert_eq!(
            render_template(r#"{{ name | default("ada") | upper }}"#, &ctx),
            "ADA"
        );
    }

    #[test]
    fn template_unknown_filter_leaves_placeholder_verbatim() {
        let ctx = json!({"name": "ada"});
        assert_eq!(
            render_template("{{ name | bogus }}", &ctx),
            "{{ name | bogus }}"
        );
    }

    /// Backend whose `await_completion` blocks until cancelled, so we can
    /// verify the executor short-circuits a mid-action run when its cancel
    /// token fires.
    struct SlowBackend;

    #[async_trait]
    impl ExecutionBackend for SlowBackend {
        async fn create_session(
            &self,
            _executor: &str,
            _prompt: &str,
            _params: &Value,
        ) -> Result<SessionId, BackendError> {
            Ok(SessionId("slow-session".into()))
        }
        async fn follow_up(
            &self,
            _session_id: SessionId,
            _executor: Option<&str>,
            _prompt: &str,
        ) -> Result<ExecutionId, BackendError> {
            Ok(ExecutionId("slow-exec".into()))
        }
        async fn start_review(
            &self,
            _session_id: SessionId,
            _executor: &str,
            _prompt: &str,
        ) -> Result<ExecutionId, BackendError> {
            Ok(ExecutionId("slow-exec".into()))
        }
        async fn merge(&self, _session_id: SessionId) -> Result<MergeOutcome, BackendError> {
            Ok(MergeOutcome::Merged {
                commit_sha: "n/a".into(),
            })
        }
        async fn await_completion(
            &self,
            _execution_id: ExecutionId,
        ) -> Result<ExecutionOutput, BackendError> {
            // Effectively forever for test purposes — long enough that cancel
            // will land before this resolves.
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(ExecutionOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
                last_assistant_message: None,
            })
        }
    }

    fn slow_follow_up_procedure() -> Procedure {
        let mut states = IndexMap::new();
        states.insert(
            "plan".into(),
            State {
                action: Some(Action::CreateSession {
                    executor: "c".into(),
                    prompt: "p".into(),
                }),
                on_success: Some("work".into()),
                ..Default::default()
            },
        );
        states.insert(
            "work".into(),
            State {
                action: Some(Action::FollowUp {
                    session_ref: "plan".into(),
                    executor: None,
                    prompt: "do the long thing".into(),
                }),
                on_success: Some("done".into()),
                ..Default::default()
            },
        );
        states.insert(
            "done".into(),
            State {
                terminal: Some(Terminal::Success),
                ..Default::default()
            },
        );
        Procedure {
            name: "slow".into(),
            version: 1,
            description: String::new(),
            triggers: Default::default(),
            initial_state: "plan".into(),
            states,
        }
    }

    #[tokio::test]
    async fn cancel_token_short_circuits_mid_action_to_cancelled() {
        let store = InMemoryRunStore::new();
        let executor = ProcedureExecutor::new(SlowBackend, store.clone(), default_gates());
        let cancel = CancellationToken::new();
        let cancel_for_signal = cancel.clone();
        let run_id = RunId::new();

        let handle = tokio::spawn(async move {
            executor
                .run_with_id_cancellable(
                    run_id,
                    &slow_follow_up_procedure(),
                    json!({}),
                    Some(cancel),
                )
                .await
        });

        // Yield long enough that `plan` has completed and `work` is parked
        // inside `await_completion`'s 60s sleep.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel_for_signal.cancel();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("cancellation should propagate within 1s")
            .expect("task panicked")
            .expect("executor returned Err");
        assert_eq!(outcome, RunOutcome::Cancelled);
        assert_eq!(store.terminal(run_id), Some(RunOutcome::Cancelled));
    }

    #[tokio::test]
    async fn cancel_token_pre_signaled_returns_cancelled_without_running() {
        let store = InMemoryRunStore::new();
        let executor = ProcedureExecutor::new(SlowBackend, store.clone(), default_gates());
        let cancel = CancellationToken::new();
        cancel.cancel(); // already cancelled before run starts

        let run_id = RunId::new();
        let outcome = executor
            .run_with_id_cancellable(run_id, &slow_follow_up_procedure(), json!({}), Some(cancel))
            .await
            .expect("executor should not error on pre-cancelled token");
        assert_eq!(outcome, RunOutcome::Cancelled);
        assert_eq!(store.terminal(run_id), Some(RunOutcome::Cancelled));
    }
}
