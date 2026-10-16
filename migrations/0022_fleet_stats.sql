-- One sample a minute of the whole service, for the landing page's graphs. The
-- control plane writes the current minute once (a second writer in the same
-- minute changes nothing) and keeps a day of them. Usage columns are null when
-- no running server reported that resource in the minute.
CREATE TABLE "public"."fleet_stats" (
    "minute" timestamptz PRIMARY KEY,
    "servers_online" integer NOT NULL,
    "players_online" integer NOT NULL,
    "cpu_used_millis" bigint,
    "cpu_total_millis" bigint,
    "memory_used_bytes" bigint,
    "memory_total_bytes" bigint,
    "tps" double precision,
    CONSTRAINT "fleet_stats_minute_check" CHECK ("minute" = date_trunc('minute', "minute")),
    CONSTRAINT "fleet_stats_counts_check" CHECK ("servers_online" >= 0 AND "players_online" >= 0),
    CONSTRAINT "fleet_stats_usage_check" CHECK (
        coalesce("cpu_used_millis", 0) >= 0 AND coalesce("cpu_total_millis", 1) > 0
        AND coalesce("memory_used_bytes", 0) >= 0 AND coalesce("memory_total_bytes", 1) > 0
    ),
    CONSTRAINT "fleet_stats_tps_check" CHECK ("tps" IS NULL OR ("tps" >= 0 AND "tps" <= 20))
);

COMMENT ON TABLE "public"."fleet_stats" IS
    'Per-minute totals across all account-owned servers and live workers; one day is kept.';
