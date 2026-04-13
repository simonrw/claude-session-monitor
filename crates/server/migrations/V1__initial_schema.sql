CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    cwd TEXT NOT NULL,
    status TEXT NOT NULL,
    status_tool TEXT,
    waiting_reason TEXT,
    waiting_detail TEXT,
    updated_at TEXT NOT NULL
);
