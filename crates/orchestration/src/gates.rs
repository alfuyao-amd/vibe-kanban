use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use tokio::{process::Command, sync::mpsc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    pub passed: bool,
    pub output: Option<String>,
    pub response: Option<Value>,
    pub summary: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("failed to spawn gate command: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("unsupported pass_when expression: `{0}`")]
    UnsupportedExpression(String),
    #[error("llm judge response missing path `{0}`")]
    MissingResponsePath(String),
    #[error("no llm judge response available")]
    MissingResponse,
    #[error("human approval channel closed")]
    ApprovalChannelClosed,
}

#[async_trait]
pub trait GateEvaluator: Send + Sync {
    async fn evaluate(&self, ctx: &GateContext) -> Result<GateOutcome, GateError>;
}

pub struct GateContext<'a> {
    pub run: Option<String>,
    pub pass_when: &'a str,
    pub prompt: Option<String>,
    pub last_assistant_message: Option<&'a str>,
}

pub struct DeterministicGateEvaluator;

#[async_trait]
impl GateEvaluator for DeterministicGateEvaluator {
    async fn evaluate(&self, ctx: &GateContext) -> Result<GateOutcome, GateError> {
        let run = ctx.run.as_deref().unwrap_or("");
        let output = Command::new("sh").arg("-c").arg(run).output().await?;
        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = if stderr.is_empty() {
            stdout
        } else if stdout.is_empty() {
            stderr
        } else {
            format!("{stdout}\n{stderr}")
        };
        let passed = evaluate_exit_code_expr(ctx.pass_when, code)?;
        Ok(GateOutcome {
            passed,
            output: Some(combined),
            response: None,
            summary: format!("exit_code={code} pass_when=`{}`", ctx.pass_when),
        })
    }
}

fn evaluate_exit_code_expr(expr: &str, code: i32) -> Result<bool, GateError> {
    let normalized: String = expr.split_whitespace().collect::<Vec<_>>().join(" ");
    let parts: Vec<&str> = normalized.split(' ').collect();
    if parts.len() != 3 || parts[0] != "exit_code" {
        return Err(GateError::UnsupportedExpression(expr.to_string()));
    }
    let rhs: i32 = parts[2]
        .parse()
        .map_err(|_| GateError::UnsupportedExpression(expr.to_string()))?;
    match parts[1] {
        "==" => Ok(code == rhs),
        "!=" => Ok(code != rhs),
        _ => Err(GateError::UnsupportedExpression(expr.to_string())),
    }
}

#[async_trait]
pub trait LlmJudgeSource: Send + Sync {
    async fn response(&self) -> Result<Value, GateError>;
}

pub struct StaticLlmJudgeSource(pub Value);

#[async_trait]
impl LlmJudgeSource for StaticLlmJudgeSource {
    async fn response(&self) -> Result<Value, GateError> {
        Ok(self.0.clone())
    }
}

pub struct LlmJudgeGateEvaluator<S: LlmJudgeSource> {
    pub source: S,
}

impl<S: LlmJudgeSource> LlmJudgeGateEvaluator<S> {
    pub fn new(source: S) -> Self {
        Self { source }
    }
}

#[async_trait]
impl<S: LlmJudgeSource> GateEvaluator for LlmJudgeGateEvaluator<S> {
    async fn evaluate(&self, ctx: &GateContext) -> Result<GateOutcome, GateError> {
        let response = self.source.response().await?;
        let passed = evaluate_llm_expr(ctx.pass_when, &response)?;
        Ok(GateOutcome {
            passed,
            output: None,
            response: Some(response),
            summary: format!("llm_judge pass_when=`{}`", ctx.pass_when),
        })
    }
}

/// Judge that parses the preceding action's `last_assistant_message` as JSON.
/// Accepts either a clean JSON document or a JSON object embedded in prose
/// (e.g. a reviewer's reply with commentary around the `{...}`).
pub struct ContextLlmJudge;

#[async_trait]
impl GateEvaluator for ContextLlmJudge {
    async fn evaluate(&self, ctx: &GateContext) -> Result<GateOutcome, GateError> {
        let raw = ctx
            .last_assistant_message
            .ok_or(GateError::MissingResponse)?;
        let response = extract_json_object(raw).ok_or_else(|| {
            GateError::UnsupportedExpression(format!(
                "llm_judge: reviewer output is not parseable JSON: {raw}"
            ))
        })?;
        let passed = evaluate_llm_expr(ctx.pass_when, &response)?;
        Ok(GateOutcome {
            passed,
            output: Some(raw.to_string()),
            response: Some(response),
            summary: format!("llm_judge pass_when=`{}`", ctx.pass_when),
        })
    }
}

fn extract_json_object(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    let bytes = trimmed.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0
                    && let Ok(v) = serde_json::from_str::<Value>(&trimmed[start..=i])
                {
                    return Some(v);
                }
            }
            _ => {}
        }
    }
    None
}

fn evaluate_llm_expr(expr: &str, response: &Value) -> Result<bool, GateError> {
    let normalized: String = expr.split_whitespace().collect::<Vec<_>>().join(" ");
    let parts: Vec<&str> = normalized.splitn(3, ' ').collect();
    if parts.len() != 3 {
        return Err(GateError::UnsupportedExpression(expr.to_string()));
    }
    let lhs = parts[0];
    let op = parts[1];
    let rhs_raw = parts[2];
    let path = lhs
        .strip_prefix("response.")
        .ok_or_else(|| GateError::UnsupportedExpression(expr.to_string()))?;
    let rhs =
        strip_quotes(rhs_raw).ok_or_else(|| GateError::UnsupportedExpression(expr.to_string()))?;
    let actual = walk_json(response, path)
        .ok_or_else(|| GateError::MissingResponsePath(path.to_string()))?;
    let actual_str = match actual {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match op {
        "==" => Ok(actual_str == rhs),
        "!=" => Ok(actual_str != rhs),
        _ => Err(GateError::UnsupportedExpression(expr.to_string())),
    }
}

fn strip_quotes(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"'))
    {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

fn walk_json<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResult {
    Approved,
    Rejected,
}

#[async_trait]
pub trait HumanApprovalSource: Send + Sync {
    async fn wait_for_approval(&self, prompt: &str) -> Result<ApprovalResult, GateError>;
}

pub struct AutoApprove(pub ApprovalResult);

#[async_trait]
impl HumanApprovalSource for AutoApprove {
    async fn wait_for_approval(&self, _prompt: &str) -> Result<ApprovalResult, GateError> {
        Ok(self.0.clone())
    }
}

pub struct ChannelApproval {
    rx: Arc<Mutex<mpsc::Receiver<ApprovalResult>>>,
}

impl ChannelApproval {
    pub fn new(rx: mpsc::Receiver<ApprovalResult>) -> Self {
        Self {
            rx: Arc::new(Mutex::new(rx)),
        }
    }
}

#[async_trait]
impl HumanApprovalSource for ChannelApproval {
    async fn wait_for_approval(&self, _prompt: &str) -> Result<ApprovalResult, GateError> {
        loop {
            let maybe = {
                let mut guard = self.rx.lock().expect("approval channel mutex poisoned");
                guard.try_recv()
            };
            match maybe {
                Ok(result) => return Ok(result),
                Err(mpsc::error::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err(GateError::ApprovalChannelClosed);
                }
            }
        }
    }
}

pub struct HumanGateEvaluator<H: HumanApprovalSource> {
    pub source: H,
}

impl<H: HumanApprovalSource> HumanGateEvaluator<H> {
    pub fn new(source: H) -> Self {
        Self { source }
    }
}

#[async_trait]
impl<H: HumanApprovalSource> GateEvaluator for HumanGateEvaluator<H> {
    async fn evaluate(&self, ctx: &GateContext) -> Result<GateOutcome, GateError> {
        let prompt = ctx.prompt.as_deref().unwrap_or("");
        let result = self.source.wait_for_approval(prompt).await?;
        let passed = matches!(result, ApprovalResult::Approved);
        Ok(GateOutcome {
            passed,
            output: None,
            response: None,
            summary: if passed {
                "human: approved".to_string()
            } else {
                "human: rejected".to_string()
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn deterministic_gate_passes_on_zero_exit() {
        let gate = DeterministicGateEvaluator;
        let ctx = GateContext {
            run: Some("echo hello".to_string()),
            pass_when: "exit_code == 0",
            prompt: None,
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(outcome.passed);
        assert!(outcome.output.as_deref().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn deterministic_gate_fails_on_nonzero_exit() {
        let gate = DeterministicGateEvaluator;
        let ctx = GateContext {
            run: Some("exit 3".to_string()),
            pass_when: "exit_code == 0",
            prompt: None,
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(!outcome.passed);
    }

    #[tokio::test]
    async fn deterministic_gate_exact_exit_code_match() {
        let gate = DeterministicGateEvaluator;
        let ctx = GateContext {
            run: Some("exit 7".to_string()),
            pass_when: "exit_code == 7",
            prompt: None,
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(outcome.passed);
    }

    #[tokio::test]
    async fn llm_judge_gate_passes_when_path_matches() {
        let source = StaticLlmJudgeSource(json!({"verdict": "pass", "feedback": ""}));
        let gate = LlmJudgeGateEvaluator::new(source);
        let ctx = GateContext {
            run: None,
            pass_when: "response.verdict == 'pass'",
            prompt: None,
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(outcome.passed);
    }

    #[tokio::test]
    async fn llm_judge_gate_fails_when_path_mismatches() {
        let source = StaticLlmJudgeSource(json!({"verdict": "fail"}));
        let gate = LlmJudgeGateEvaluator::new(source);
        let ctx = GateContext {
            run: None,
            pass_when: "response.verdict == 'pass'",
            prompt: None,
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(!outcome.passed);
    }

    #[tokio::test]
    async fn llm_judge_gate_walks_nested_path() {
        let source = StaticLlmJudgeSource(json!({"review": {"status": "ok"}}));
        let gate = LlmJudgeGateEvaluator::new(source);
        let ctx = GateContext {
            run: None,
            pass_when: "response.review.status == 'ok'",
            prompt: None,
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(outcome.passed);
    }

    #[tokio::test]
    async fn human_gate_auto_approve_passes() {
        let gate = HumanGateEvaluator::new(AutoApprove(ApprovalResult::Approved));
        let ctx = GateContext {
            run: None,
            pass_when: "",
            prompt: Some("approve?".to_string()),
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(outcome.passed);
    }

    #[tokio::test]
    async fn human_gate_auto_reject_fails() {
        let gate = HumanGateEvaluator::new(AutoApprove(ApprovalResult::Rejected));
        let ctx = GateContext {
            run: None,
            pass_when: "",
            prompt: Some("approve?".to_string()),
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(!outcome.passed);
    }

    #[tokio::test]
    async fn context_judge_parses_direct_json_and_passes() {
        let gate = ContextLlmJudge;
        let raw = r#"{"verdict": "pass", "feedback": "lgtm"}"#;
        let ctx = GateContext {
            run: None,
            pass_when: "response.verdict == 'pass'",
            prompt: None,
            last_assistant_message: Some(raw),
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(outcome.passed);
        assert_eq!(
            outcome.response.unwrap().get("feedback").unwrap(),
            &json!("lgtm")
        );
    }

    #[tokio::test]
    async fn context_judge_extracts_json_embedded_in_prose() {
        let gate = ContextLlmJudge;
        let raw = "Here is my review.\n\n```json\n{\"verdict\": \"fail\", \"feedback\": \"tests missing\"}\n```\nHope that helps.";
        let ctx = GateContext {
            run: None,
            pass_when: "response.verdict == 'pass'",
            prompt: None,
            last_assistant_message: Some(raw),
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(!outcome.passed);
        assert_eq!(
            outcome.response.unwrap().get("verdict").unwrap(),
            &json!("fail")
        );
    }

    #[tokio::test]
    async fn context_judge_errors_when_no_message() {
        let gate = ContextLlmJudge;
        let ctx = GateContext {
            run: None,
            pass_when: "response.verdict == 'pass'",
            prompt: None,
            last_assistant_message: None,
        };
        let err = gate.evaluate(&ctx).await.unwrap_err();
        assert!(matches!(err, GateError::MissingResponse));
    }

    #[tokio::test]
    async fn context_judge_errors_when_no_json_present() {
        let gate = ContextLlmJudge;
        let ctx = GateContext {
            run: None,
            pass_when: "response.verdict == 'pass'",
            prompt: None,
            last_assistant_message: Some("I reviewed it and it looks fine."),
        };
        let err = gate.evaluate(&ctx).await.unwrap_err();
        assert!(matches!(err, GateError::UnsupportedExpression(_)));
    }

    #[tokio::test]
    async fn human_gate_channel_delivers_approval() {
        let (tx, rx) = mpsc::channel(1);
        tx.send(ApprovalResult::Approved).await.unwrap();
        let gate = HumanGateEvaluator::new(ChannelApproval::new(rx));
        let ctx = GateContext {
            run: None,
            pass_when: "",
            prompt: Some("approve?".to_string()),
            last_assistant_message: None,
        };
        let outcome = gate.evaluate(&ctx).await.unwrap();
        assert!(outcome.passed);
    }
}
