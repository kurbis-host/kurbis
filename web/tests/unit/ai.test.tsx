import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AiAssistant } from "../../components/ai";
import {
  aiModelName,
  aiModelOption,
  aiSettingsFromForm,
  aiSettingsToForm,
  aiToolCallStatus,
  formatToolInput,
  isActiveAiJob,
  isAiBudgetSpent,
  nextAiPeriodStart,
  withAiJob,
} from "../../lib/ai";
import type { components } from "../../lib/api/generated";
import { queryKeys } from "../../lib/query";
import { activeAiJob, aiOverview, finishedAiJob, server } from "./fixtures";

type AiJobDetail = components["schemas"]["AiJobDetail"];
type AiOverview = components["schemas"]["AiOverview"];
type AiSettings = components["schemas"]["AiSettings"];

type RecordedRequest = { method: string; path: string; body: unknown };

/** Answers every API request with `respond` and records what the panel sent. */
function stubApi(respond: (request: RecordedRequest) => Response) {
  const requests: RecordedRequest[] = [];
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : new Request(new URL(String(input), "http://localhost"), init);
    const text = await request.text();
    const recorded = { method: request.method, path: new URL(request.url).pathname, body: text ? JSON.parse(text) : undefined };
    requests.push(recorded);
    return respond(recorded);
  }));
  return requests;
}

function renderAssistant(overview: AiOverview = aiOverview) {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false }, mutations: { retry: false } },
  });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  }
  render(
    <AiAssistant serverId={server.id} initialOverview={overview} renderedAt={Date.parse("2026-10-05T18:10:00Z")} />,
    { wrapper: Wrapper },
  );
  return client;
}

function jobRow(prompt: string) {
  return screen.getByText(prompt).closest("li")!;
}

const budgetInput = () => screen.getByRole("textbox", { name: "Monthly budget in US dollars" });

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("AI request helpers", () => {
  it("treats only queued, thinking and acting requests as still in progress", () => {
    for (const state of ["queued", "running", "awaiting_tools"] as const) expect(isActiveAiJob(state), state).toBe(true);
    for (const state of ["succeeded", "failed", "cancelled"] as const) expect(isActiveAiJob(state), state).toBe(false);
  });

  it("describes where each tool call is", () => {
    expect(aiToolCallStatus({ state: "pending", is_error: false })).toEqual({ label: "Waiting for the server", tone: "progress" });
    expect(aiToolCallStatus({ state: "delivered", is_error: false })).toEqual({ label: "Running in game", tone: "progress" });
    expect(aiToolCallStatus({ state: "completed", is_error: false })).toEqual({ label: "Done", tone: "good" });
    expect(aiToolCallStatus({ state: "completed", is_error: true })).toEqual({ label: "Error", tone: "danger" });
  });

  it("replaces one request of the overview without touching the others", () => {
    const cancelled = { ...activeAiJob, state: "cancelled" as const };

    expect(withAiJob(aiOverview, cancelled).jobs).toEqual([cancelled, finishedAiJob]);
    expect(aiOverview.jobs).toEqual([activeAiJob, finishedAiJob]);
  });

  it("names models, falling back to the id of one that is no longer offered", () => {
    expect(aiModelName(aiOverview.models, "claude-haiku-5-5")).toBe("Claude Haiku 5.5");
    expect(aiModelName(aiOverview.models, "claude-retired-1")).toBe("claude-retired-1");
  });

  it("lists models with their prices, and unpriced models as free", () => {
    expect(aiModelOption(aiOverview.models[2])).toBe("Claude Haiku 5.5 · $0.10 / $0.50");
    expect(aiModelOption({ id: "llama3.1:8b", name: "llama3.1:8b", input_micros_per_mtok: 0, output_micros_per_mtok: 0 })).toBe(
      "llama3.1:8b · free",
    );
    expect(aiModelOption({ id: "gpt-oss-120b", name: "gpt-oss-120b", input_micros_per_mtok: 0, output_micros_per_mtok: 600_000 })).toBe(
      "gpt-oss-120b · $0.00 / $0.60",
    );
  });

  it("resets budgets at the start of the next UTC month", () => {
    expect(nextAiPeriodStart("2026-10-01T00:00:00Z")).toBe("2026-11-01T00:00:00.000Z");
    expect(nextAiPeriodStart("2026-12-01T00:00:00Z")).toBe("2027-01-01T00:00:00.000Z");
  });

  it("counts a budget as used up once spending reaches it", () => {
    expect(isAiBudgetSpent({ spent_micros: 4_999_999, reserved_micros: 1, budget_micros: 5_000_000 })).toBe(false);
    expect(isAiBudgetSpent({ spent_micros: 5_000_000, reserved_micros: 0, budget_micros: 5_000_000 })).toBe(true);
  });

  it("shows tool input as indented JSON and hides empty input", () => {
    expect(formatToolInput({ entity: "pig", count: 2 })).toBe('{\n  "entity": "pig",\n  "count": 2\n}');
    expect(formatToolInput(["Alex"])).toBe('[\n  "Alex"\n]');
    expect(formatToolInput("night")).toBe('"night"');
    for (const input of [null, undefined, {}]) expect(formatToolInput(input)).toBeNull();
  });

  it("maps settings to form values and back, with empty choices meaning the defaults", () => {
    const custom: AiSettings = { enabled: false, model: "claude-haiku-5-5", effort: "low", monthly_budget_micros: 12_500_000 };

    expect(aiSettingsToForm(custom, aiOverview.models)).toEqual({ enabled: false, model: "claude-haiku-5-5", effort: "low", monthly_budget: "12.50" });
    expect(aiSettingsToForm(aiOverview.settings, aiOverview.models)).toEqual({ enabled: true, model: "", effort: "", monthly_budget: "" });
    for (const settings of [custom, aiOverview.settings]) {
      expect(aiSettingsFromForm(aiSettingsToForm(settings, aiOverview.models))).toEqual(settings);
    }
    expect(aiSettingsToForm({ ...custom, model: "claude-retired-1" }, aiOverview.models).model).toBe("");
  });
});

describe("AI assistant page", () => {
  it("meters this month's spending against the server and account budgets", () => {
    renderAssistant();

    expect(screen.getByText("Since October 1 · resets November 1 (UTC)")).toBeVisible();
    expect(screen.getByText("Server budget").parentElement).toHaveTextContent("$1.25 of $5.00");
    expect(screen.getByRole("progressbar", { name: "Server budget used" })).toHaveAttribute("aria-valuenow", "33");
    expect(screen.getByText("Account budget").parentElement).toHaveTextContent("$3.10 of $20.00");
    expect(screen.getByRole("progressbar", { name: "Account budget used" })).toHaveAttribute("aria-valuenow", "18");
    expect(screen.getAllByText(/^Up to \$0\.40 is held for requests in progress\./)).toHaveLength(2);
    expect(screen.queryByText(/^Used up/)).not.toBeInTheDocument();
    expect(screen.getByText("Input tokens").parentElement).toHaveTextContent("1,234,567");
    expect(screen.getByText("Cache reads").parentElement).toHaveTextContent("900,000");
  });

  it("says when a budget is used up", () => {
    renderAssistant({
      ...aiOverview,
      usage: { ...aiOverview.usage, server: { spent_micros: 5_000_000, reserved_micros: 0, budget_micros: 5_000_000 } },
    });

    expect(screen.getByRole("progressbar", { name: "Server budget used" })).toHaveAttribute("aria-valuenow", "100");
    expect(screen.getByText("Used up: new requests are refused until it resets.")).toBeVisible();
  });

  it("shows operators the in-game commands", () => {
    renderAssistant();

    const hint = screen.getByRole("heading", { name: "Use it in game" }).parentElement!;
    for (const command of ["/ai ask <question>", "/ai new <question>", "/ai status", "/ai cancel"]) {
      expect(within(hint).getByText(command)).toBeVisible();
    }
    expect(within(hint).getByText(/follow-up within an hour continues the same conversation/)).toBeVisible();
  });

  it("explains that the assistant isn't set up but still lists usage and past requests", () => {
    renderAssistant({
      available: false,
      settings: { enabled: true, model: null, effort: null, monthly_budget_micros: null },
      models: [],
      usage: {
        ...aiOverview.usage,
        server: { spent_micros: 1_250_000, reserved_micros: 0, budget_micros: 0 },
        account: { spent_micros: 3_100_000, reserved_micros: 0, budget_micros: 0 },
      },
      jobs: [finishedAiJob],
    });

    expect(screen.getByText(/isn't set up on this Kurbis installation/)).toBeVisible();
    expect(screen.getByText("Server budget").parentElement).toHaveTextContent("$1.25 spent");
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Settings" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Use it in game" })).not.toBeInTheDocument();
    expect(within(jobRow("give everyone night vision")).getByText(/· claude-sonnet-5-5$/)).toBeVisible();
  });

  it("warns that operators' requests are refused while the assistant is off", () => {
    renderAssistant({ ...aiOverview, settings: { ...aiOverview.settings, enabled: false } });

    expect(screen.getByText(/turned off for this server/)).toBeVisible();
    expect(screen.getByRole("switch", { name: "Enabled" })).not.toBeChecked();
  });

  it("saves settings, with empty choices sent as the defaults", async () => {
    const saved: AiOverview = {
      ...aiOverview,
      settings: { enabled: false, model: "claude-haiku-5-5", effort: "high", monthly_budget_micros: 12_500_000 },
    };
    const requests = stubApi(() => Response.json(saved));
    const client = renderAssistant();
    const user = userEvent.setup();

    expect(screen.getByRole("option", { name: "Default (Claude Sonnet 5.5)" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Claude Haiku 5.5 · $0.10 / $0.50" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Default (Medium)" })).toBeInTheDocument();
    expect(budgetInput()).toHaveAttribute("placeholder", "5.00");
    expect(screen.getByText(/^Leave empty for the default of \$5\.00\. The account allows \$20\.00 a month/)).toBeVisible();
    expect(screen.getByRole("button", { name: "Save settings" })).toBeDisabled();

    await user.click(screen.getByRole("switch", { name: "Enabled" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Model" }), "claude-haiku-5-5");
    await user.selectOptions(screen.getByRole("combobox", { name: "Effort" }), "high");
    await user.type(budgetInput(), "12.5");
    expect(screen.getByText("Unsaved draft")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    await waitFor(() => expect(screen.getByText("Everything is saved.")).toBeVisible());
    expect(requests).toEqual([{
      method: "PUT",
      path: `/api/v1/servers/${server.id}/ai/settings`,
      body: { enabled: false, model: "claude-haiku-5-5", effort: "high", monthly_budget_micros: 12_500_000 },
    }]);
    expect(budgetInput()).toHaveValue("12.50");
    expect(screen.getByText(/turned off for this server/)).toBeVisible();
    expect(client.getQueryData(queryKeys.ai(server.id))).toEqual(saved);
  });

  it("refuses a server budget above the account's before sending anything", async () => {
    const requests = stubApi(() => Response.json(aiOverview));
    renderAssistant();
    const user = userEvent.setup();

    await user.type(budgetInput(), "25");
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("A server can't spend more than the account budget of $20.00.");
    expect(requests).toEqual([]);
  });

  it("shows why the control plane refused the settings and keeps the draft", async () => {
    stubApi(() => Response.json({ code: "invalid_model", message: "That model isn't offered anymore." }, { status: 422 }));
    renderAssistant();
    const user = userEvent.setup();

    await user.selectOptions(screen.getByRole("combobox", { name: "Model" }), "claude-opus-5-5");
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("That model isn't offered anymore.");
    expect(screen.getByRole("combobox", { name: "Model" })).toHaveValue("claude-opus-5-5");
    expect(screen.getByText("Unsaved draft")).toBeVisible();
  });

  it("follows live setting changes until the form holds a draft", async () => {
    const client = renderAssistant();
    const user = userEvent.setup();
    const effort = screen.getByRole("combobox", { name: "Effort" });

    await act(async () => {
      client.setQueryData(queryKeys.ai(server.id), { ...aiOverview, settings: { ...aiOverview.settings, effort: "low" } });
    });
    await waitFor(() => expect(effort).toHaveValue("low"));

    await user.type(budgetInput(), "7");
    await act(async () => {
      client.setQueryData(queryKeys.ai(server.id), {
        ...aiOverview,
        settings: { ...aiOverview.settings, effort: "high" },
        usage: { ...aiOverview.usage, server: { ...aiOverview.usage.server, spent_micros: 1_300_000 } },
      });
    });
    await waitFor(() => expect(screen.getByText("Server budget").parentElement).toHaveTextContent("$1.30 of $5.00"));
    expect(effort).toHaveValue("low");
    expect(budgetInput()).toHaveValue("7");
    expect(screen.getByText("Unsaved draft")).toBeVisible();
  });

  it("lists recent requests and cancels only the ones still in progress", async () => {
    const cancelled = { ...activeAiJob, state: "cancelled" as const, finished_at: "2026-10-05T18:06:00Z" };
    const requests = stubApi(() => Response.json(cancelled));
    renderAssistant();
    const user = userEvent.setup();

    const active = jobRow("summon a pig next to me");
    expect(within(active).getByText("Acting in game")).toBeVisible();
    expect(within(active).getByText("Steve")).toBeVisible();
    expect(within(active).getByText("$0.0042 · 1 model call · 1 tool call · Claude Sonnet 5.5")).toBeVisible();
    const finished = jobRow("give everyone night vision");
    expect(within(finished).getByText("Answered")).toBeVisible();
    expect(within(finished).getByText("Everyone has night vision for five minutes.")).toBeVisible();
    expect(within(finished).getByText("$0.01 · 2 model calls · 1 tool call · Claude Sonnet 5.5")).toBeVisible();
    expect(within(finished).queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();

    await user.click(within(active).getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(within(active).getByText("Cancelled")).toBeVisible());
    expect(within(active).queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
    expect(requests).toEqual([
      { method: "POST", path: `/api/v1/servers/${server.id}/ai/jobs/${activeAiJob.id}/cancel`, body: undefined },
    ]);
  });

  it("loads a request's tool calls once, when it is first expanded", async () => {
    const detail: AiJobDetail = {
      job: finishedAiJob,
      tool_calls: [
        {
          call_id: "call-1",
          name: "run_command",
          input: { command: "effect give @a night_vision 300" },
          output: "Applied effect Night Vision to 2 targets",
          is_error: false,
          state: "completed",
          created_at: "2026-10-05T18:00:03Z",
          completed_at: "2026-10-05T18:00:04Z",
        },
        {
          call_id: "call-2",
          name: "list_players",
          input: {},
          output: "The server didn't answer in time.",
          is_error: true,
          state: "completed",
          created_at: "2026-10-05T18:00:05Z",
          completed_at: "2026-10-05T18:00:08Z",
        },
      ],
    };
    const requests = stubApi(() => Response.json(detail));
    renderAssistant();
    const user = userEvent.setup();

    const row = jobRow("give everyone night vision");
    const toggle = within(row).getByRole("button", { name: "Show tool calls" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    await user.click(toggle);

    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(toggle).toHaveAccessibleName("Hide tool calls");
    const [command, players] = await within(row).findAllByRole("listitem");
    expect(within(command).getByText("run_command")).toBeVisible();
    expect(within(command).getByText("Done")).toBeVisible();
    expect(within(command).getByText('{ "command": "effect give @a night_vision 300" }')).toBeVisible();
    expect(within(command).getByText("Result")).toBeVisible();
    expect(within(command).getByText("Applied effect Night Vision to 2 targets")).toBeVisible();
    expect(within(players).getByText("list_players")).toBeVisible();
    expect(within(players).queryByText("Input")).not.toBeInTheDocument();
    expect(within(players).getAllByText("Error")).toHaveLength(2);
    expect(within(players).getByText("The server didn't answer in time.")).toBeVisible();

    await user.click(toggle);
    await user.click(toggle);
    expect(within(row).getAllByRole("listitem")).toHaveLength(2);
    expect(requests).toEqual([
      { method: "GET", path: `/api/v1/servers/${server.id}/ai/jobs/${finishedAiJob.id}`, body: undefined },
    ]);
  });
});
