import type { components } from "../../lib/api/generated";

export type ServerFilter = components["schemas"]["AdminServerFilter"];

/** The server list's state filters; shared by the server page (to read `?filter=`) and the client list. */
export const serverFilters: { value: ServerFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "online", label: "Online" },
  { value: "sleeping", label: "Sleeping" },
  { value: "queued", label: "Queued" },
  { value: "failed", label: "Failed" },
  { value: "stuck", label: "Stuck" },
  { value: "never_started", label: "Never started" },
];

export function isServerFilter(value: string | undefined): value is ServerFilter {
  return serverFilters.some((filter) => filter.value === value);
}
