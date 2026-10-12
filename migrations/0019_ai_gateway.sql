-- AI gateway: per-server settings, operator conversations, agent jobs, tool
-- calls executed by the in-game plugin, chat events for the requesting
-- operator, and the usage ledger budgets are enforced against.
--
-- Transcript columns hold the exact JSON text exchanged with the model
-- provider. Replayed conversations must stay byte-identical so preserved
-- thinking blocks remain valid and the prompt cache keeps hitting.

ALTER TABLE "public"."server_events"
    DROP CONSTRAINT "server_events_event_type_check";

ALTER TABLE "public"."server_events"
    ADD CONSTRAINT "server_events_event_type_check"
    CHECK (("event_type" = ANY (ARRAY[
        'server'::text,
        'operation'::text,
        'snapshot'::text,
        'log'::text,
        'players'::text,
        'whitelist'::text,
        'operators'::text,
        'ai'::text
    ])));

-- A missing row means "gateway defaults": enabled, default model and effort,
-- default monthly budget.
CREATE TABLE "public"."ai_server_settings" (
    "server_id" uuid PRIMARY KEY REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "enabled" boolean NOT NULL DEFAULT true,
    "model" text,
    "effort" text,
    "monthly_budget_micros" bigint,
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ai_server_settings_model_check"
        CHECK ("model" IS NULL OR char_length("model") BETWEEN 1 AND 64),
    CONSTRAINT "ai_server_settings_effort_check"
        CHECK ("effort" IS NULL OR "effort" IN ('low', 'medium', 'high')),
    CONSTRAINT "ai_server_settings_budget_check"
        CHECK ("monthly_budget_micros" IS NULL OR "monthly_budget_micros" >= 0)
);

-- The model, effort, system prompt and tool declarations are frozen when a
-- conversation starts; later turns replay them verbatim.
CREATE TABLE "public"."ai_conversations" (
    "id" uuid PRIMARY KEY,
    "server_id" uuid NOT NULL REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "operator_id" uuid NOT NULL,
    "model" text NOT NULL,
    "effort" text NOT NULL,
    "system_json" text NOT NULL,
    "tools_json" text NOT NULL,
    "message_count" integer NOT NULL DEFAULT 0,
    "closed_at" timestamptz,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ai_conversations_model_check" CHECK (char_length("model") BETWEEN 1 AND 64),
    CONSTRAINT "ai_conversations_effort_check" CHECK ("effort" IN ('low', 'medium', 'high')),
    CONSTRAINT "ai_conversations_message_count_check" CHECK ("message_count" >= 0)
);

CREATE INDEX "ai_conversations_open_idx"
    ON "public"."ai_conversations" ("server_id", "operator_id", "updated_at" DESC)
    WHERE "closed_at" IS NULL;
CREATE INDEX "ai_conversations_updated_at_idx"
    ON "public"."ai_conversations" ("updated_at");

CREATE TABLE "public"."ai_messages" (
    "conversation_id" uuid NOT NULL REFERENCES "public"."ai_conversations"("id") ON DELETE CASCADE,
    "seq" integer NOT NULL,
    "role" text NOT NULL,
    "content_json" text NOT NULL,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY ("conversation_id", "seq"),
    CONSTRAINT "ai_messages_seq_check" CHECK ("seq" >= 0),
    CONSTRAINT "ai_messages_role_check" CHECK ("role" IN ('user', 'assistant'))
);

-- One operator request. A job alternates between model calls (running, under
-- a lease) and tool rounds the plugin executes (awaiting_tools). While a model
-- call is in flight its worst-case cost is reserved until reserved_until.
CREATE TABLE "public"."ai_jobs" (
    "id" uuid PRIMARY KEY,
    "server_id" uuid NOT NULL REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "conversation_id" uuid NOT NULL REFERENCES "public"."ai_conversations"("id") ON DELETE CASCADE,
    "operator_id" uuid NOT NULL,
    "operator_name" text NOT NULL,
    "prompt" text NOT NULL,
    "state" text NOT NULL DEFAULT 'queued',
    "answer" text,
    "error" text,
    "model_calls" integer NOT NULL DEFAULT 0,
    "tool_calls" integer NOT NULL DEFAULT 0,
    "cost_micros" bigint NOT NULL DEFAULT 0,
    "reserved_micros" bigint NOT NULL DEFAULT 0,
    "reserved_until" timestamptz,
    "lease_token" uuid,
    "lease_expires_at" timestamptz,
    "stalls" integer NOT NULL DEFAULT 0,
    "deadline_at" timestamptz NOT NULL,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    "finished_at" timestamptz,
    CONSTRAINT "ai_jobs_operator_name_check" CHECK (char_length("operator_name") BETWEEN 1 AND 64),
    CONSTRAINT "ai_jobs_prompt_check" CHECK (char_length("prompt") BETWEEN 1 AND 2000),
    CONSTRAINT "ai_jobs_state_check" CHECK ("state" IN (
        'queued', 'running', 'awaiting_tools', 'succeeded', 'failed', 'cancelled'
    )),
    CONSTRAINT "ai_jobs_counters_check" CHECK (
        "model_calls" >= 0 AND "tool_calls" >= 0 AND "stalls" >= 0
        AND "cost_micros" >= 0 AND "reserved_micros" >= 0
    ),
    CONSTRAINT "ai_jobs_lease_check" CHECK (
        ("state" = 'running') = ("lease_token" IS NOT NULL AND "lease_expires_at" IS NOT NULL)
    ),
    CONSTRAINT "ai_jobs_finished_check" CHECK (
        ("state" IN ('succeeded', 'failed', 'cancelled')) = ("finished_at" IS NOT NULL)
    )
);

CREATE UNIQUE INDEX "ai_jobs_active_operator_idx"
    ON "public"."ai_jobs" ("server_id", "operator_id")
    WHERE "state" IN ('queued', 'running', 'awaiting_tools');
CREATE INDEX "ai_jobs_queue_idx"
    ON "public"."ai_jobs" ("updated_at", "id")
    WHERE "state" = 'queued';
CREATE INDEX "ai_jobs_lease_idx"
    ON "public"."ai_jobs" ("lease_expires_at")
    WHERE "state" = 'running';
CREATE INDEX "ai_jobs_deadline_idx"
    ON "public"."ai_jobs" ("deadline_at")
    WHERE "state" IN ('queued', 'running', 'awaiting_tools');
CREATE INDEX "ai_jobs_reservation_idx"
    ON "public"."ai_jobs" ("server_id", "reserved_until")
    WHERE "reserved_micros" > 0;
CREATE INDEX "ai_jobs_server_history_idx"
    ON "public"."ai_jobs" ("server_id", "created_at" DESC, "id" DESC);
CREATE INDEX "ai_jobs_conversation_idx"
    ON "public"."ai_jobs" ("conversation_id");

-- Tool calls requested by the model. The plugin receives pending calls through
-- the sync endpoint (delivered) and returns their results (completed).
CREATE TABLE "public"."ai_tool_calls" (
    "job_id" uuid NOT NULL REFERENCES "public"."ai_jobs"("id") ON DELETE CASCADE,
    "call_id" text NOT NULL,
    "server_id" uuid NOT NULL REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "turn" integer NOT NULL,
    "seq" integer NOT NULL,
    "name" text NOT NULL,
    "input_json" text NOT NULL,
    "state" text NOT NULL DEFAULT 'pending',
    "output" text,
    "is_error" boolean NOT NULL DEFAULT false,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "delivered_at" timestamptz,
    "completed_at" timestamptz,
    PRIMARY KEY ("job_id", "call_id"),
    CONSTRAINT "ai_tool_calls_call_id_check" CHECK (char_length("call_id") BETWEEN 1 AND 128),
    CONSTRAINT "ai_tool_calls_name_check" CHECK (char_length("name") BETWEEN 1 AND 64),
    CONSTRAINT "ai_tool_calls_order_check" CHECK ("turn" >= 1 AND "seq" >= 0),
    CONSTRAINT "ai_tool_calls_state_check" CHECK ("state" IN ('pending', 'delivered', 'completed')),
    CONSTRAINT "ai_tool_calls_completion_check" CHECK (
        ("state" = 'completed') = ("completed_at" IS NOT NULL AND "output" IS NOT NULL)
    )
);

CREATE INDEX "ai_tool_calls_open_idx"
    ON "public"."ai_tool_calls" ("server_id", "created_at")
    WHERE "state" <> 'completed';

-- Chat lines for the requesting operator, read by the plugin with a cursor.
CREATE TABLE "public"."ai_job_events" (
    "id" bigserial PRIMARY KEY,
    "job_id" uuid NOT NULL REFERENCES "public"."ai_jobs"("id") ON DELETE CASCADE,
    "server_id" uuid NOT NULL REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "kind" text NOT NULL,
    "message" text NOT NULL,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ai_job_events_kind_check" CHECK ("kind" IN ('progress', 'answer', 'error')),
    CONSTRAINT "ai_job_events_message_check" CHECK (char_length("message") BETWEEN 1 AND 8000)
);

CREATE INDEX "ai_job_events_server_idx"
    ON "public"."ai_job_events" ("server_id", "id");

-- Append-only usage ledger: one row per model attempt, priced when recorded.
-- Rows outlive their server and job so a deleted server cannot reset the
-- account budget.
CREATE TABLE "public"."ai_usage" (
    "id" bigserial PRIMARY KEY,
    "owner_id" uuid NOT NULL REFERENCES "public"."users"("id") ON DELETE CASCADE,
    "server_id" uuid REFERENCES "public"."servers"("id") ON DELETE SET NULL,
    "job_id" uuid REFERENCES "public"."ai_jobs"("id") ON DELETE SET NULL,
    "model" text NOT NULL,
    "input_tokens" bigint NOT NULL,
    "output_tokens" bigint NOT NULL,
    "cache_creation_input_tokens" bigint NOT NULL,
    "cache_read_input_tokens" bigint NOT NULL,
    "cost_micros" bigint NOT NULL,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ai_usage_model_check" CHECK (char_length("model") BETWEEN 1 AND 128),
    CONSTRAINT "ai_usage_tokens_check" CHECK (
        "input_tokens" >= 0 AND "output_tokens" >= 0
        AND "cache_creation_input_tokens" >= 0 AND "cache_read_input_tokens" >= 0
    ),
    CONSTRAINT "ai_usage_cost_check" CHECK ("cost_micros" >= 0)
);

CREATE INDEX "ai_usage_owner_idx" ON "public"."ai_usage" ("owner_id", "created_at");
CREATE INDEX "ai_usage_server_idx" ON "public"."ai_usage" ("server_id", "created_at");
CREATE INDEX "ai_usage_created_at_idx" ON "public"."ai_usage" ("created_at");

CREATE FUNCTION "public"."emit_ai_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    PERFORM emit_server_event(NEW.server_id, 'ai');
    RETURN NEW;
END;
$$;

CREATE TRIGGER "ai_jobs_panel_event"
    AFTER INSERT OR UPDATE OF "state", "model_calls", "tool_calls", "cost_micros"
    ON "public"."ai_jobs"
    FOR EACH ROW EXECUTE FUNCTION "public"."emit_ai_event"();

CREATE TRIGGER "ai_server_settings_panel_event"
    AFTER INSERT OR UPDATE ON "public"."ai_server_settings"
    FOR EACH ROW EXECUTE FUNCTION "public"."emit_ai_event"();

COMMENT ON TABLE "public"."ai_usage" IS
    'Append-only AI usage ledger; monthly server and account budgets are sums over this table.';
COMMENT ON COLUMN "public"."ai_jobs"."reserved_micros" IS
    'Worst-case cost of the in-flight model call, counted against budgets until reserved_until.';
