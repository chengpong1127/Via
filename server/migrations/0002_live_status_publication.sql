CREATE TABLE IF NOT EXISTS via_provider_aliases (
    provider TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    authority_namespace TEXT NOT NULL,
    upstream_key TEXT NOT NULL,
    via_id TEXT NOT NULL,
    valid_from TIMESTAMPTZ NOT NULL,
    valid_until TIMESTAMPTZ,
    PRIMARY KEY (provider, entity_kind, authority_namespace, upstream_key, valid_from)
);

ALTER TABLE via_network_snapshots
    ADD COLUMN IF NOT EXISTS selected_observation_ids JSONB NOT NULL DEFAULT '[]'::jsonb;

CREATE INDEX IF NOT EXISTS via_provider_alias_lookup_idx
    ON via_provider_aliases (provider, entity_kind, authority_namespace, upstream_key, valid_from DESC);
