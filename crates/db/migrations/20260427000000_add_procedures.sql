CREATE TABLE procedures (
    id            BLOB PRIMARY KEY,
    project_id    BLOB NOT NULL,
    name          TEXT NOT NULL,
    version       INTEGER NOT NULL DEFAULT 1,
    yaml          TEXT NOT NULL,
    source        TEXT NOT NULL DEFAULT 'user',
    created_at    TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    UNIQUE (project_id, name)
);

CREATE INDEX idx_procedures_project_id ON procedures(project_id);
