import type { components } from "./api/generated";

type ObservedState = components["schemas"]["ObservedState"];
type Server = components["schemas"]["Server"];

export type LifecyclePresentation = {
  label: string;
  short: string;
  detail: string;
  tone: "quiet" | "progress" | "good" | "warning" | "danger";
};

export const lifecycle: Record<ObservedState, LifecyclePresentation> = {
  sleeping: { label: "Sleeping", short: "World saved. Infrastructure released.", detail: "Your latest snapshot is safe. Connecting through the game gateway or pressing Start wakes the world.", tone: "quiet" },
  queued: { label: "Queued", short: "Waiting for capacity.", detail: "Your place is held. Kurbis will begin restoring the world as soon as capacity is available.", tone: "progress" },
  restoring: { label: "Restoring", short: "Bringing the latest world snapshot onto a worker.", detail: "Storage is being prepared before the Minecraft runtime starts.", tone: "progress" },
  starting: { label: "Starting", short: "Pumpkin is warming up.", detail: "The runtime is launching and the gateway will publish it once it is ready for players.", tone: "progress" },
  online: { label: "Online", short: "Ready for players.", detail: "The gateway is routing connections to the live runtime. The idle timer starts when the last player leaves.", tone: "good" },
  stopping: { label: "Stopping", short: "Closing the runtime cleanly.", detail: "New work is blocked while Kurbis prepares a final consistent world snapshot.", tone: "warning" },
  snapshotting: { label: "Snapshotting", short: "Saving the authoritative world state.", detail: "The worker stays reserved until the snapshot is complete and safe to restore.", tone: "progress" },
  resetting: { label: "Resetting", short: "Preparing a fresh world generation.", detail: "The runtime is fenced while Kurbis backs up, resets, and validates the replacement world.", tone: "warning" },
  failed: { label: "Needs attention", short: "The last lifecycle operation failed.", detail: "Review the issue below. Retry is available when the failure is recoverable.", tone: "danger" },
  deleted: { label: "Deleted", short: "This hosted world has been removed.", detail: "Its address and runtime allocation are no longer active.", tone: "quiet" },
};

export function lifecycleActions(server: Server) {
  const state = server.observed_state;
  return {
    canStart: state === "sleeping" || (state === "failed" && Boolean(server.error?.retryable)),
    canStop: ["queued", "restoring", "starting", "online", "failed"].includes(state),
    canRestart: state === "online",
    canEdit: state === "sleeping",
    // The API takes a whitelist change while sleeping or online; a running server applies it live.
    canChangeAccess: state === "sleeping" || state === "online",
    canSnapshot: state === "online",
    canRestore: state === "sleeping" || state === "online",
    canDelete: state === "sleeping",
    canReset: state === "sleeping" || state === "online",
  };
}
