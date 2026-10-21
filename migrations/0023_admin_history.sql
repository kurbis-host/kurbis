-- History for the operator dashboard. The landing page's per-minute samples of
-- the whole service are kept for 30 days instead of one; each running server
-- gets a sample a minute of its own, kept for 7 days; and every day is rolled
-- up into one row that is kept for good.

COMMENT ON TABLE "public"."fleet_stats" IS
    'Per-minute totals across all account-owned servers and live workers; 30 days are kept.';

-- One sample a minute of each running server, written with the fleet sample of
-- the same minute. Usage columns are null when the server reported no fresh
-- sample of its current generation in that minute.
CREATE TABLE "public"."server_runtime_samples" (
    "server_id" uuid NOT NULL REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "minute" timestamptz NOT NULL,
    "players_online" integer NOT NULL,
    "cpu_used_millis" bigint,
    "memory_used_bytes" bigint,
    "tps" double precision,
    PRIMARY KEY ("server_id", "minute"),
    CONSTRAINT "server_runtime_samples_minute_check" CHECK ("minute" = date_trunc('minute', "minute")),
    CONSTRAINT "server_runtime_samples_usage_check" CHECK (
        "players_online" >= 0 AND coalesce("cpu_used_millis", 0) >= 0
        AND coalesce("memory_used_bytes", 0) >= 0
    ),
    CONSTRAINT "server_runtime_samples_tps_check" CHECK ("tps" IS NULL OR ("tps" >= 0 AND "tps" <= 20))
);

CREATE INDEX "server_runtime_samples_minute_idx" ON "public"."server_runtime_samples" ("minute");

COMMENT ON TABLE "public"."server_runtime_samples" IS
    'Per-minute samples of each running server for the operator dashboard; 7 days are kept.';

-- One row per UTC day. A day's figures only ever grow: a later refresh that
-- counts fewer rows (a server deleted since, say) keeps the larger count.
CREATE TABLE "public"."daily_stats" (
    "day" date PRIMARY KEY,
    "signups" integer NOT NULL DEFAULT 0,
    "servers_created" integer NOT NULL DEFAULT 0,
    -- Successful start operations requested that day, and the servers they woke.
    "wakes" integer NOT NULL DEFAULT 0,
    "servers_woken" integer NOT NULL DEFAULT 0,
    -- Highest per-minute fleet sample of the day; null for a day nobody sampled.
    "peak_servers_online" integer,
    "peak_players_online" integer,
    "ai_cost_micros" bigint NOT NULL DEFAULT 0,
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "daily_stats_counts_check" CHECK (
        "signups" >= 0 AND "servers_created" >= 0 AND "wakes" >= 0 AND "servers_woken" >= 0
        AND coalesce("peak_servers_online", 0) >= 0 AND coalesce("peak_players_online", 0) >= 0
        AND "ai_cost_micros" >= 0
    )
);

COMMENT ON TABLE "public"."daily_stats" IS
    'One row per UTC day of signups, servers, wakes, peaks and AI spend; kept for good.';

-- The operator dashboard reads a server's operations newest first, failures by
-- when they failed, and wakes by when they were asked for.
CREATE INDEX "operations_server_history_idx"
    ON "public"."operations" ("server_id", "created_at" DESC);
CREATE INDEX "operations_failed_idx"
    ON "public"."operations" ("updated_at") WHERE "status" = 'failed';
CREATE INDEX "operations_wake_history_idx"
    ON "public"."operations" ("created_at") WHERE "kind" = 'start' AND "status" = 'succeeded';
CREATE INDEX "users_created_at_idx" ON "public"."users" ("created_at");

-- Recount the UTC days from `first_day` to `last_day` from the rows that still
-- exist. Both the backfill below and the control plane's per-minute refresh of
-- today and yesterday use it.
CREATE FUNCTION "public"."refresh_daily_stats"("first_day" date, "last_day" date) RETURNS void
    LANGUAGE "sql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
    WITH days AS (
        SELECT day::date AS day FROM generate_series(first_day, last_day, interval '1 day') AS day
    ), bounds AS (
        SELECT day, (day::timestamp AT TIME ZONE 'UTC') AS starts,
               ((day + 1)::timestamp AT TIME ZONE 'UTC') AS ends
        FROM days
    )
    INSERT INTO daily_stats(day, signups, servers_created, wakes, servers_woken,
                            peak_servers_online, peak_players_online, ai_cost_micros, updated_at)
    SELECT bounds.day,
           (SELECT count(*) FROM users WHERE created_at >= bounds.starts AND created_at < bounds.ends),
           (SELECT count(*) FROM servers WHERE created_at >= bounds.starts AND created_at < bounds.ends),
           wakes.wakes, wakes.servers_woken,
           peaks.servers_online, peaks.players_online,
           (SELECT coalesce(sum(cost_micros), 0) FROM ai_usage
            WHERE created_at >= bounds.starts AND created_at < bounds.ends),
           now()
    FROM bounds
    CROSS JOIN LATERAL (
        SELECT count(*) AS wakes, count(DISTINCT server_id) AS servers_woken
        FROM operations
        WHERE kind = 'start' AND status = 'succeeded'
          AND created_at >= bounds.starts AND created_at < bounds.ends
    ) wakes
    CROSS JOIN LATERAL (
        SELECT max(servers_online) AS servers_online, max(players_online) AS players_online
        FROM fleet_stats WHERE minute >= bounds.starts AND minute < bounds.ends
    ) peaks
    ON CONFLICT (day) DO UPDATE SET
        signups = greatest(daily_stats.signups, EXCLUDED.signups),
        servers_created = greatest(daily_stats.servers_created, EXCLUDED.servers_created),
        wakes = greatest(daily_stats.wakes, EXCLUDED.wakes),
        servers_woken = greatest(daily_stats.servers_woken, EXCLUDED.servers_woken),
        peak_servers_online = greatest(daily_stats.peak_servers_online, EXCLUDED.peak_servers_online),
        peak_players_online = greatest(daily_stats.peak_players_online, EXCLUDED.peak_players_online),
        ai_cost_micros = greatest(daily_stats.ai_cost_micros, EXCLUDED.ai_cost_micros),
        updated_at = now();
$$;

-- Every day since the first account, from what is still there.
SELECT refresh_daily_stats(
    coalesce((SELECT min(created_at AT TIME ZONE 'UTC')::date FROM users),
             (now() AT TIME ZONE 'UTC')::date),
    (now() AT TIME ZONE 'UTC')::date
);
