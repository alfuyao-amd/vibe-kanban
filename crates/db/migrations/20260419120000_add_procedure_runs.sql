CREATE TABLE procedure_runs (
    id                BLOB PRIMARY KEY,
    project_id        BLOB NOT NULL,
    procedure_name    TEXT NOT NULL,
    procedure_version INTEGER NOT NULL,
    current_state     TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'running',
    params            TEXT NOT NULL DEFAULT '{}',
    state_history     TEXT NOT NULL DEFAULT '[]',
    workspace_id      BLOB,
    created_at        TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at        TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE SET NULL
);

CREATE INDEX idx_procedure_runs_project_id ON procedure_runs(project_id);
CREATE INDEX idx_procedure_runs_status ON procedure_runs(status);
CREATE INDEX idx_procedure_runs_project_status ON procedure_runs(project_id, status);
