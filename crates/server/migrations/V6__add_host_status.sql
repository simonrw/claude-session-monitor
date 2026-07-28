CREATE TABLE IF NOT EXISTS host_status (
    hostname TEXT NOT NULL,
    agent_kind TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    PRIMARY KEY (hostname, agent_kind)
);
