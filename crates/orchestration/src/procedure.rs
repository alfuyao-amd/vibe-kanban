use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Default value to use when this param is absent from the run's
    /// initial params. Applied during run start, before templating. Must
    /// match `ty` (validated when the procedure loads).
    #[serde(default)]
    pub default: Option<Value>,
}

impl ParamSpec {
    /// Returns `true` when the given JSON value is compatible with this
    /// param's declared `ty`. `null` is always considered compatible
    /// (treated as "absent" by the run-start validator).
    pub fn matches_type(&self, value: &Value) -> bool {
        match (&self.ty, value) {
            (_, Value::Null) => true,
            (ParamType::String, Value::String(_)) => true,
            (ParamType::Integer, Value::Number(n)) => n.is_i64() || n.is_u64(),
            (ParamType::Boolean, Value::Bool(_)) => true,
            (ParamType::Array, Value::Array(arr)) => match &self.items {
                None => true,
                Some(item_ty) => arr.iter().all(|v| {
                    let probe = ParamSpec {
                        ty: item_ty.clone(),
                        required: false,
                        description: None,
                        items: None,
                        default: None,
                    };
                    probe.matches_type(v)
                }),
            },
            _ => false,
        }
    }
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
    #[error("param `{name}` has a default that doesn't match its declared type")]
    DefaultTypeMismatch { name: String },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParamError {
    #[error("missing required param `{0}`")]
    MissingRequired(String),
    #[error("param `{name}` expected type {expected:?}, got {actual}")]
    TypeMismatch {
        name: String,
        expected: ParamType,
        actual: String,
    },
}

impl Procedure {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.states.contains_key(&self.initial_state) {
            return Err(ValidationError::MissingInitialState(
                self.initial_state.clone(),
            ));
        }
        for (name, spec) in &self.triggers.params {
            if let Some(default) = &spec.default
                && !spec.matches_type(default)
            {
                return Err(ValidationError::DefaultTypeMismatch { name: name.clone() });
            }
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

    /// Validate the run's incoming params against the procedure's declared
    /// trigger spec, filling in declared defaults for missing optional params.
    /// Returns the merged params object on success. The caller is expected to
    /// pass the merged value forward as the run's initial context.
    ///
    /// Behaviour:
    /// - Required params must be present and non-null; otherwise `MissingRequired`.
    /// - Present params must match their declared type; otherwise `TypeMismatch`.
    /// - Absent (or null) optional params with a `default` get filled.
    /// - Unknown keys in `params` are passed through untouched (so callers
    ///   can carry contextual data the procedure didn't declare, e.g. the
    ///   workspace block injected by the run-start handler).
    pub fn validate_and_fill_params(&self, params: Value) -> Result<Value, ParamError> {
        let mut obj = match params {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                let mut m = serde_json::Map::new();
                m.insert("value".to_string(), other);
                m
            }
        };

        for (name, spec) in &self.triggers.params {
            let present = obj.get(name).is_some_and(|v| !v.is_null());
            if !present {
                if spec.required && spec.default.is_none() {
                    return Err(ParamError::MissingRequired(name.clone()));
                }
                if let Some(default) = &spec.default {
                    obj.insert(name.clone(), default.clone());
                }
                continue;
            }
            let value = &obj[name];
            if !spec.matches_type(value) {
                return Err(ParamError::TypeMismatch {
                    name: name.clone(),
                    expected: spec.ty.clone(),
                    actual: type_name(value).to_string(),
                });
            }
        }
        Ok(Value::Object(obj))
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn proc_with_params(params: IndexMap<String, ParamSpec>) -> Procedure {
        let mut states = IndexMap::new();
        states.insert(
            "start".to_string(),
            State {
                terminal: Some(Terminal::Success),
                ..Default::default()
            },
        );
        Procedure {
            name: "p".to_string(),
            version: 1,
            description: String::new(),
            triggers: Triggers {
                match_hints: vec![],
                params,
            },
            initial_state: "start".to_string(),
            states,
        }
    }

    fn spec(ty: ParamType, required: bool, default: Option<Value>) -> ParamSpec {
        ParamSpec {
            ty,
            required,
            description: None,
            items: None,
            default,
        }
    }

    #[test]
    fn validate_rejects_default_with_wrong_type() {
        let mut params = IndexMap::new();
        params.insert(
            "count".to_string(),
            spec(ParamType::Integer, false, Some(json!("not-a-number"))),
        );
        let proc = proc_with_params(params);
        assert_eq!(
            proc.validate(),
            Err(ValidationError::DefaultTypeMismatch {
                name: "count".into()
            })
        );
    }

    #[test]
    fn validate_and_fill_rejects_missing_required() {
        let mut params = IndexMap::new();
        params.insert("goal".to_string(), spec(ParamType::String, true, None));
        let proc = proc_with_params(params);
        let err = proc.validate_and_fill_params(json!({})).unwrap_err();
        assert_eq!(err, ParamError::MissingRequired("goal".into()));
    }

    #[test]
    fn validate_and_fill_uses_default_when_missing_optional() {
        let mut params = IndexMap::new();
        params.insert(
            "test_command".to_string(),
            spec(ParamType::String, false, Some(json!("npm test"))),
        );
        let proc = proc_with_params(params);
        let merged = proc.validate_and_fill_params(json!({})).unwrap();
        assert_eq!(merged["test_command"], json!("npm test"));
    }

    #[test]
    fn validate_and_fill_caller_value_wins_over_default() {
        let mut params = IndexMap::new();
        params.insert(
            "test_command".to_string(),
            spec(ParamType::String, false, Some(json!("npm test"))),
        );
        let proc = proc_with_params(params);
        let merged = proc
            .validate_and_fill_params(json!({"test_command": "yarn test"}))
            .unwrap();
        assert_eq!(merged["test_command"], json!("yarn test"));
    }

    #[test]
    fn validate_and_fill_rejects_type_mismatch() {
        let mut params = IndexMap::new();
        params.insert("count".to_string(), spec(ParamType::Integer, true, None));
        let proc = proc_with_params(params);
        let err = proc
            .validate_and_fill_params(json!({"count": "twelve"}))
            .unwrap_err();
        assert!(matches!(err, ParamError::TypeMismatch { .. }));
    }

    #[test]
    fn validate_and_fill_passes_through_unknown_keys() {
        let proc = proc_with_params(IndexMap::new());
        let merged = proc
            .validate_and_fill_params(json!({"workspace_id": "abc"}))
            .unwrap();
        assert_eq!(merged["workspace_id"], json!("abc"));
    }

    #[test]
    fn matches_type_array_with_items_validates_each_element() {
        let array_spec = spec(ParamType::Array, false, Some(json!(["a", "b"])));
        let array_spec = ParamSpec {
            items: Some(ParamType::String),
            ..array_spec
        };
        assert!(array_spec.matches_type(&json!(["one", "two"])));
        assert!(!array_spec.matches_type(&json!(["one", 2])));
    }
}
