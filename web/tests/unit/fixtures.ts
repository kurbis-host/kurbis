import type { components } from "../../lib/api/generated";

type AiJob = components["schemas"]["AiJob"];
type AiOverview = components["schemas"]["AiOverview"];
type Server = components["schemas"]["Server"];
type Session = components["schemas"]["Session"];
type Snapshot = components["schemas"]["Snapshot"];

export const snapshot: Snapshot = {
  generation: 4,
  reason: "shutdown",
  size_bytes: 256 * 1024 * 1024,
  created_at: "2026-09-30T18:00:00Z",
};

export const server: Server = {
  id: "server-1",
  subdomain: "melon-world",
  is_default: true,
  java_address: "melon-world.mc.example.test",
  bedrock_address: "melon-world.mc.example.test",
  desired_state: "stopped",
  observed_state: "sleeping",
  settings: {
    motd: "Hosted by Kurbis",
    difficulty: "Normal",
    gamemode: "Survival",
    view_distance: 12,
    simulation_distance: 8,
    hardcore: false,
    whitelist: false,
    tab_stats: false,
  },
  storage_bytes: 512 * 1024 * 1024,
  storage_limit_bytes: 5 * 1024 * 1024 * 1024,
  memory_limit_bytes: 2 * 1024 * 1024 * 1024,
  player_count: 0,
  generation: 0,
  created_at: "2026-09-29T18:00:00Z",
  updated_at: "2026-09-30T18:00:00Z",
  current_operation: null,
  latest_snapshot: snapshot,
  error: null,
};

export const session: Session = {
  user: {
    id: "user-1",
    email: "player@example.test",
    display_name: "Browser Player",
    account_status: "active",
  },
  limits: {
    memory_bytes: 2 * 1024 * 1024 * 1024,
    storage_bytes: 5 * 1024 * 1024 * 1024,
    idle_seconds: 300,
    player_limit: null,
  },
  root_domain: "kurbis.host",
};

export const finishedAiJob: AiJob = {
  id: "00000000-0000-4000-8000-0000000000a1",
  conversation_id: "00000000-0000-4000-8000-0000000000c1",
  operator_id: "00000000-0000-4000-8000-000000000001",
  operator_name: "Alex",
  prompt: "give everyone night vision",
  model: "claude-sonnet-5-5",
  state: "succeeded",
  answer: "Everyone has night vision for five minutes.",
  model_calls: 2,
  tool_calls: 1,
  cost_micros: 12_400,
  created_at: "2026-10-05T18:00:00Z",
  updated_at: "2026-10-05T18:00:09Z",
  finished_at: "2026-10-05T18:00:09Z",
};

export const activeAiJob: AiJob = {
  id: "00000000-0000-4000-8000-0000000000a2",
  conversation_id: "00000000-0000-4000-8000-0000000000c2",
  operator_id: "00000000-0000-4000-8000-000000000002",
  operator_name: "Steve",
  prompt: "summon a pig next to me",
  model: "claude-sonnet-5-5",
  state: "awaiting_tools",
  model_calls: 1,
  tool_calls: 1,
  cost_micros: 4_200,
  created_at: "2026-10-05T18:05:00Z",
  updated_at: "2026-10-05T18:05:03Z",
};

export const aiOverview: AiOverview = {
  available: true,
  settings: { enabled: true, model: null, effort: null, monthly_budget_micros: null },
  defaults: {
    model: "claude-sonnet-5-5",
    effort: "medium",
    server_monthly_budget_micros: 5_000_000,
    account_monthly_budget_micros: 20_000_000,
  },
  models: [
    { id: "claude-opus-5-5", name: "Claude Opus 5.5", input_micros_per_mtok: 4_000_000, output_micros_per_mtok: 20_000_000 },
    { id: "claude-sonnet-5-5", name: "Claude Sonnet 5.5", input_micros_per_mtok: 2_000_000, output_micros_per_mtok: 10_000_000 },
    { id: "claude-haiku-5-5", name: "Claude Haiku 5.5", input_micros_per_mtok: 100_000, output_micros_per_mtok: 500_000 },
  ],
  usage: {
    period_start: "2026-10-01T00:00:00Z",
    server: { spent_micros: 1_250_000, reserved_micros: 400_000, budget_micros: 5_000_000 },
    account: { spent_micros: 3_100_000, reserved_micros: 400_000, budget_micros: 20_000_000 },
    model_calls: 42,
    input_tokens: 1_234_567,
    output_tokens: 85_210,
    cache_creation_input_tokens: 12_000,
    cache_read_input_tokens: 900_000,
  },
  jobs: [activeAiJob, finishedAiJob],
};
