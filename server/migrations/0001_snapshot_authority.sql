CREATE TABLE IF NOT EXISTS via_live_status_observations (
    id TEXT PRIMARY KEY,
    family TEXT NOT NULL CHECK (family IN ('arrival_estimate', 'service_condition', 'bike_availability')),
    observed_payload JSONB NOT NULL,
    measured_at TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    received_at TIMESTAMPTZ NOT NULL,
    normalized_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS via_feed_health (
    id BIGSERIAL PRIMARY KEY,
    capability TEXT NOT NULL,
    state TEXT NOT NULL,
    reason TEXT,
    evaluated_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS via_network_snapshots (
    id TEXT PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL,
    live_cutoff TIMESTAMPTZ NOT NULL,
    static_network_revision TEXT NOT NULL,
    schedule_revision TEXT NOT NULL,
    freshness_policy_version TEXT NOT NULL,
    publication_version BIGINT NOT NULL UNIQUE CHECK (publication_version > 0),
    feed_evidence JSONB NOT NULL DEFAULT '[]'::jsonb
);

CREATE TABLE IF NOT EXISTS via_refresh_control (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    current_snapshot_id TEXT REFERENCES via_network_snapshots(id),
    publication_version BIGINT NOT NULL DEFAULT 0,
    lease_key TEXT,
    lease_token BIGINT,
    lease_expires_at TIMESTAMPTZ
);

INSERT INTO via_refresh_control (singleton)
VALUES (TRUE)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS via_refresh_cycles (
    refresh_key TEXT PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('published')),
    snapshot_id TEXT REFERENCES via_network_snapshots(id),
    completed_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS via_live_status_expiry_idx
    ON via_live_status_observations (expires_at);
CREATE INDEX IF NOT EXISTS via_feed_health_expiry_idx
    ON via_feed_health (expires_at);
