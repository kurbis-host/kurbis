ALTER TABLE public.ci_runtime_placements
    ADD COLUMN bootstrap_pending boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN public.ci_runtime_placements.world_initialized IS
    'Whether this runtime generation crossed the world mutation boundary.';
COMMENT ON COLUMN public.ci_runtime_placements.bootstrap_pending IS
    'This generation has begun bootstrap but has not published its first durable world checkpoint.';
