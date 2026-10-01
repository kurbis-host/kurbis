import { describe, expect, it } from "vitest";
import { lifecycle, lifecycleActions } from "../../lib/lifecycle";
import { server } from "./fixtures";

describe("lifecycle presentation", () => {
  it("has designed copy for every backend lifecycle state", () => {
    expect(Object.keys(lifecycle)).toEqual([
      "sleeping", "queued", "restoring", "starting", "online", "stopping", "snapshotting", "resetting", "failed", "deleted",
    ]);
    for (const presentation of Object.values(lifecycle)) {
      expect(presentation.label.length).toBeGreaterThan(2);
      expect(presentation.detail.length).toBeGreaterThan(20);
    }
  });

  it("enforces sleeping edits, online snapshots, and state-safe actions", () => {
    expect(lifecycleActions(server)).toMatchObject({ canStart: true, canEdit: true, canSnapshot: false, canRestore: true });
    expect(lifecycleActions({ ...server, observed_state: "online", desired_state: "running" })).toMatchObject({ canStart: false, canStop: true, canRestart: true, canEdit: false, canSnapshot: true, canRestore: false });
    expect(lifecycleActions({ ...server, observed_state: "restoring", desired_state: "running" })).toMatchObject({ canStart: false, canStop: true, canRestart: false, canEdit: false });
    expect(lifecycleActions({ ...server, observed_state: "resetting", desired_state: "running" })).toMatchObject({ canStart: false, canStop: false, canEdit: false, canReset: false });
  });
});
