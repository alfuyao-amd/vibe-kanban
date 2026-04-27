CREATE TABLE project_lead_agents (
    project_id    BLOB PRIMARY KEY,
    session_id    BLOB NOT NULL,
    workspace_id  BLOB NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    FOREIGN KEY (project_id)   REFERENCES projects(id)   ON DELETE CASCADE,
    FOREIGN KEY (session_id)   REFERENCES sessions(id)   ON DELETE CASCADE,
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);
