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

pub fn builtin_procedures() -> Result<Vec<Procedure>, LoadError> {
    Ok(vec![
        load_from_yaml(FEATURE_WITH_TESTS)?,
        load_from_yaml(SMOKE_SUCCESS)?,
    ])
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
