import type { components } from "./api/generated";
import { formatUsd, parseUsdInput, usdInputValue } from "./format";
import type { LifecyclePresentation } from "./lifecycle";
import type { AiSettingsFormValues } from "./schemas";

type AiBudget = components["schemas"]["AiBudget"];
type AiEffort = components["schemas"]["AiEffort"];
type AiJob = components["schemas"]["AiJob"];
type AiJobState = components["schemas"]["AiJobState"];
type AiModel = components["schemas"]["AiModel"];
type AiOverview = components["schemas"]["AiOverview"];
type AiSettings = components["schemas"]["AiSettings"];
type AiToolCall = components["schemas"]["AiToolCall"];

export type AiStatus = { label: string; tone: LifecyclePresentation["tone"] };

export const aiJobStates: Record<AiJobState, AiStatus & { active: boolean }> = {
  queued: { label: "Queued", tone: "progress", active: true },
  running: { label: "Thinking", tone: "progress", active: true },
  awaiting_tools: { label: "Acting in game", tone: "progress", active: true },
  succeeded: { label: "Answered", tone: "good", active: false },
  failed: { label: "Failed", tone: "danger", active: false },
  cancelled: { label: "Cancelled", tone: "quiet", active: false },
};

/** Active requests can still change and be cancelled; finished ones never change again. */
export function isActiveAiJob(state: AiJobState) {
  return aiJobStates[state].active;
}

export function aiToolCallStatus(call: Pick<AiToolCall, "state" | "is_error">): AiStatus {
  if (call.state === "pending") return { label: "Waiting for the server", tone: "progress" };
  if (call.state === "delivered") return { label: "Running in game", tone: "progress" };
  return call.is_error ? { label: "Error", tone: "danger" } : { label: "Done", tone: "good" };
}

export const aiEfforts: Record<AiEffort, string> = { low: "Low", medium: "Medium", high: "High" };

export function aiModelName(models: AiModel[], id: string) {
  return models.find((model) => model.id === id)?.name ?? id;
}

/** A model with its prices per million input / output tokens; unpriced models are not charged. */
export function aiModelOption(model: AiModel) {
  const { name, input_micros_per_mtok: input, output_micros_per_mtok: output } = model;
  if (input === 0 && output === 0) return `${name} · free`;
  return `${name} · ${formatUsd(input)} / ${formatUsd(output)}`;
}

/** Replace one request of the overview with a newer copy of it. */
export function withAiJob(overview: AiOverview, job: AiJob): AiOverview {
  return { ...overview, jobs: overview.jobs.map((item) => (item.id === job.id ? job : item)) };
}

/** Budgets are counted per UTC calendar month. */
export function nextAiPeriodStart(periodStart: string) {
  const start = new Date(periodStart);
  return new Date(Date.UTC(start.getUTCFullYear(), start.getUTCMonth() + 1, 1)).toISOString();
}

/** New requests are refused once spending reaches the cap. */
export function isAiBudgetSpent(budget: AiBudget) {
  return budget.spent_micros >= budget.budget_micros;
}

/** Tool input as readable JSON; tools that take no arguments show nothing. */
export function formatToolInput(input: unknown) {
  if (input == null) return null;
  if (typeof input === "object" && !Array.isArray(input) && Object.keys(input).length === 0) return null;
  return JSON.stringify(input, null, 2);
}

/** Form values for saved settings. A model that is no longer offered shows as the default it falls back to. */
export function aiSettingsToForm(settings: AiSettings, models: AiModel[]): AiSettingsFormValues {
  return {
    enabled: settings.enabled,
    model: settings.model && models.some((model) => model.id === settings.model) ? settings.model : "",
    effort: settings.effort ?? "",
    monthly_budget: settings.monthly_budget_micros == null ? "" : usdInputValue(settings.monthly_budget_micros),
  };
}

export function aiSettingsFromForm(values: AiSettingsFormValues): AiSettings {
  return {
    enabled: values.enabled,
    model: values.model || null,
    effort: values.effort || null,
    monthly_budget_micros: parseUsdInput(values.monthly_budget),
  };
}
