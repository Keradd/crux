-- Layer 4 pinned-cache flag.
--
-- Some files are part of the agent's *startup context* and should never
-- fall out of the read cache: OpenClaw / Claude Code memory bundles
-- (MEMORY.md, AGENTS.md, SOUL.md, USER.md, TOOLS.md, CLAUDE.md,
--  IDENTITY.md, HEARTBEAT.md) and any user-listed extras.
--
-- We add a `pinned` flag so:
--   1. The L4 prefetch step can mark these rows on session start.
--   2. Future eviction / LRU policies can skip pinned rows.
--   3. Telemetry can break savings down by "pinned hit" vs "regular hit".
--
-- Default 0 keeps existing rows behaving exactly as before.

ALTER TABLE read_cache ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_read_cache_pinned
    ON read_cache(pinned, project_root);
