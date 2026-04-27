use crate::procedure::{Procedure, ValidationError};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("yaml parse error: {0}")]
    Parse(#[from] serde_yaml_ng::Error),
    #[error("procedure validation failed: {0}")]
    Validation(#[from] ValidationError),
}

pub fn load_from_yaml(yaml: &str) -> Result<Procedure, LoadError> {
    let procedure: Procedure = serde_yaml_ng::from_str(yaml)?;
    procedure.validate()?;
    Ok(procedure)
}

const FEATURE_WITH_TESTS: &str = include_str!("../procedures/feature_with_tests.yaml");
const SMOKE_SUCCESS: &str = include_str!("../procedures/smoke_success.yaml");

/// Map of `name -> raw YAML` for every built-in procedure. The name keys come
/// from the YAML body (parsed lazily by [`builtin_procedure_yaml`] callers), so
/// keeping this list in sync with the YAML files just means adding a new
/// `include_str!` entry here.
const BUILTIN_YAMLS: &[(&str, &str)] = &[
    ("feature_with_tests", FEATURE_WITH_TESTS),
    ("smoke_success", SMOKE_SUCCESS),
];

pub fn builtin_procedures() -> Result<Vec<Procedure>, LoadError> {
    BUILTIN_YAMLS
        .iter()
        .map(|(_, yaml)| load_from_yaml(yaml))
        .collect()
}

/// Look up a built-in procedure's raw YAML body by name. Returns `None` for
/// unknown names. The UI's "view source / fork to project-local" path uses
/// this to populate the editor with the bundled YAML.
pub fn builtin_procedure_yaml(name: &str) -> Option<&'static str> {
    BUILTIN_YAMLS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, y)| *y)
}

/// Names of every built-in procedure, in their canonical order.
pub fn builtin_procedure_names() -> Vec<&'static str> {
    BUILTIN_YAMLS.iter().map(|(n, _)| *n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_procedures_parse_and_validate() {
        let procs = builtin_procedures().expect("built-ins must parse");
        assert!(!procs.is_empty());
        let names: Vec<_> = procs.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"feature_with_tests"));
    }

    #[test]
    fn invalid_initial_state_is_rejected() {
        let yaml = r#"
name: bad
version: 1
initial_state: nope
states:
  start:
    terminal: success
"#;
        let err = load_from_yaml(yaml).unwrap_err();
        assert!(
            matches!(err, LoadError::Validation(ValidationError::MissingInitialState(s)) if s == "nope")
        );
    }

    #[test]
    fn builtin_procedure_yaml_returns_bundled_body() {
        let yaml = builtin_procedure_yaml("feature_with_tests").expect("known builtin");
        assert!(
            yaml.contains("name: feature_with_tests"),
            "bundled YAML should match its name; got: {}",
            &yaml[..yaml.len().min(80)]
        );
        assert!(builtin_procedure_yaml("does_not_exist").is_none());
    }

    #[test]
    fn builtin_procedure_names_lists_every_builtin() {
        let names = builtin_procedure_names();
        assert!(names.contains(&"feature_with_tests"));
        assert!(names.contains(&"smoke_success"));
    }

    #[test]
    fn unknown_transition_is_rejected() {
        let yaml = r#"
name: bad
version: 1
initial_state: start
states:
  start:
    action: { kind: merge, session_ref: x }
    on_success: missing
"#;
        let err = load_from_yaml(yaml).unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validation(ValidationError::UnknownTransition { .. })
        ));
    }
}
