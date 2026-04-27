//! Project-aware procedure catalog.
//!
//! Built-in procedures (compiled in via `include_str!`) are always available
//! across every project; project-local procedures live in the `procedures`
//! table. Built-in names take precedence: a project-local procedure with the
//! same name as a built-in is ignored (and logged) — built-ins are immutable.

use db::models::procedure::ProcedureRecord;
use orchestration::Procedure;
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("procedure load: {0}")]
    Load(#[from] orchestration::LoadError),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

/// Returns built-ins followed by project-local procedures, with built-in
/// names winning on conflict.
pub async fn list_for_project(
    pool: &SqlitePool,
    project_id: Uuid,
) -> Result<Vec<Procedure>, CatalogError> {
    let mut procs = orchestration::builtin_procedures()?;
    let stored = ProcedureRecord::list_for_project(pool, project_id).await?;
    for record in stored {
        if procs.iter().any(|p| p.name == record.name) {
            tracing::warn!(
                procedure = %record.name,
                project_id = %project_id,
                "project-local procedure shadows built-in; using built-in"
            );
            continue;
        }
        match orchestration::load_from_yaml(&record.yaml) {
            Ok(p) => procs.push(p),
            Err(err) => {
                tracing::error!(
                    procedure = %record.name,
                    project_id = %project_id,
                    %err,
                    "skipping project-local procedure that fails to parse"
                );
            }
        }
    }
    Ok(procs)
}

/// Find a procedure by name within a project's catalog. Falls back to the
/// global built-in catalog (which is the same set returned by
/// `orchestration::builtin_procedures`).
pub async fn find_in_project(
    pool: &SqlitePool,
    project_id: Uuid,
    name: &str,
) -> Result<Option<Procedure>, CatalogError> {
    if let Some(builtin) = orchestration::builtin_procedures()?
        .into_iter()
        .find(|p| p.name == name)
    {
        return Ok(Some(builtin));
    }
    let record = ProcedureRecord::find_by_name(pool, project_id, name).await?;
    match record {
        Some(r) => Ok(Some(orchestration::load_from_yaml(&r.yaml)?)),
        None => Ok(None),
    }
}
