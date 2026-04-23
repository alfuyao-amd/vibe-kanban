use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Procedure {
    pub name: String,
    pub version: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: Triggers,
    pub initial_state: String,
    pub states: IndexMap<String, State>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Triggers {
    #[serde(default)]
    pub match_hints: Vec<String>,
    #[serde(default)]
    pub params: IndexMap<String, ParamSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParamSpec {
    #[serde(rename = "type")]
    pub ty: ParamType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub items: Option<ParamType>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Integer,
    Boolean,
    Array,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    #[serde(default)]
    pub action: Option<Action>,
    #[serde(default)]
    pub gate: Option<Gate>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub on_success: Option<String>,
    #[serde(default)]
    pub on_failure: Option<String>,
    #[serde(default)]
    pub terminal: Option<Terminal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    CreateSession {
        executor: String,
        prompt: String,
    },
    FollowUp {
        session_ref: String,
        #[serde(default)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Gate {
    Deterministic { run: String, pass_when: String },
    LlmJudge { pass_when: String },
    Human { prompt: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Terminal {
    Success,
    Failure,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ValidationError {
    #[error("initial_state `{0}` does not exist in states")]
    MissingInitialState(String),
    #[error("state `{from}` references unknown state `{to}` via {field}")]
    UnknownTransition {
        from: String,
        to: String,
        field: &'static str,
    },
    #[error("state `{0}` has neither an action, a gate, nor terminal")]
    EmptyState(String),
    #[error("state `{0}` is terminal but also defines transitions")]
    TerminalWithTransitions(String),
}

impl Procedure {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.states.contains_key(&self.initial_state) {
            return Err(ValidationError::MissingInitialState(
                self.initial_state.clone(),
            ));
        }
        for (name, state) in &self.states {
            if state.terminal.is_some() {
                if state.on_success.is_some() || state.on_failure.is_some() {
                    return Err(ValidationError::TerminalWithTransitions(name.clone()));
                }
                continue;
            }
            if state.action.is_none() && state.gate.is_none() {
                return Err(ValidationError::EmptyState(name.clone()));
            }
            for (field, target) in [
                ("on_success", &state.on_success),
                ("on_failure", &state.on_failure),
            ] {
                if let Some(target) = target
                    && !self.states.contains_key(target)
                {
                    return Err(ValidationError::UnknownTransition {
                        from: name.clone(),
                        to: target.clone(),
                        field,
                    });
                }
            }
        }
        Ok(())
    }
}
