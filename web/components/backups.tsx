"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Archive, Camera, RotateCcw } from "lucide-react";
import { useState } from "react";
import { apiVoid, errorMessage, jsonBody } from "../lib/api/client";
import { fetchServer, fetchSnapshots } from "../lib/api/queries";
import type { Server, Snapshot } from "../lib/contracts";
import { formatBytes, formatDateTime } from "../lib/format";
import { lifecycleActions } from "../lib/lifecycle";
import { queryKeys } from "../lib/query";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  Notice,
  PageHeader,
  Surface,
} from "./ui";

function snapshotReason(reason: Snapshot["reason"]) {
  if (reason === "manual") return "Manual backup";
  if (reason === "shutdown") return "Clean shutdown snapshot";
  if (reason === "restore") return "Restored authoritative generation";
  return "Periodic snapshot";
}

export function Backups({
  initialServer,
  initialSnapshots,
}: {
  initialServer: Server;
  initialSnapshots: Snapshot[];
}) {
  const queryClient = useQueryClient();
  const { data: server = initialServer } = useQuery({
    queryKey: queryKeys.server,
    queryFn: fetchServer,
    initialData: initialServer,
  });
  const { data: snapshots = initialSnapshots } = useQuery({
    queryKey: queryKeys.snapshots,
    queryFn: fetchSnapshots,
    initialData: initialSnapshots,
  });
  const [requestError, setRequestError] = useState<string | null>(null);

  if (!server) throw new Error("Server disappeared while backups were open.");

  const allowed = lifecycleActions(server);
  const manual = useMutation({
    mutationFn: () =>
      apiVoid("/api/v1/server/actions", {
        method: "POST",
        body: jsonBody({ action: "snapshot" }),
      }),
    onMutate: () => setRequestError(null),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.server });
      void queryClient.invalidateQueries({ queryKey: queryKeys.snapshots });
    },
    onError: (error) => setRequestError(errorMessage(error)),
  });

  async function restore(snapshot: Snapshot) {
    setRequestError(null);
    try {
      await apiVoid(`/api/v1/server/snapshots/${snapshot.generation}/restore`, {
        method: "POST",
      });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.server }),
        queryClient.invalidateQueries({ queryKey: queryKeys.snapshots }),
      ]);
    } catch (error) {
      setRequestError(errorMessage(error));
    }
  }

  return (
    <>
      <PageHeader
        title="Backups"
        description="Completed world snapshots only. Manual snapshots are available online; restoring a snapshot requires the server to be sleeping."
      />
      {requestError ? (
        <div className="mb-[18px]">
          <Notice tone="danger">{requestError}</Notice>
        </div>
      ) : null}

      <div className="grid grid-cols-[minmax(0,1fr)_320px] items-start gap-[18px] max-[860px]:grid-cols-1">
        <Surface className="px-[26px] pt-3 pb-[22px] max-[620px]:px-[18px]">
          {snapshots.length ? (
            snapshots.map((snapshot) => (
              <article
                className="relative grid grid-cols-[38px_minmax(0,1fr)_auto] items-start gap-3.5 border-b border-[#ddd4c3] py-[22px] last:border-b-0 max-[620px]:grid-cols-[34px_minmax(0,1fr)]"
                key={snapshot.generation}
              >
                <span className="grid size-[34px] place-items-center rounded-[9px] bg-[#e4dccd] text-earth">
                  <Archive size={17} />
                </span>
                <div>
                  <h2 className="m-0 font-display text-[17px] leading-[1.2] font-[800]">
                    Generation {snapshot.generation}
                  </h2>
                  <p className="mt-1.5 mb-0 text-xs leading-[1.45] text-ink-soft">
                    {snapshotReason(snapshot.reason)}
                  </p>
                  <div className="mt-[11px]">
                    <ConfirmDialog
                      disabled={!allowed.canRestore}
                      title={`Restore generation ${snapshot.generation}?`}
                      description="Restoring an older completed snapshot creates a new authoritative generation derived from it. The server must stay sleeping while the restore is requested."
                      confirmLabel="Restore snapshot"
                      onConfirm={() => restore(snapshot)}
                      trigger={
                        <Button
                          variant="secondary"
                          size="small"
                          disabled={!allowed.canRestore}
                        >
                          <RotateCcw size={14} />
                          Restore
                        </Button>
                      }
                    />
                  </div>
                </div>
                <div className="text-right text-[11px] text-ink-soft max-[620px]:col-start-2 max-[620px]:text-left">
                  <strong className="block text-[13px] text-ink">
                    {formatBytes(snapshot.size_bytes)}
                  </strong>
                  <span>{formatDateTime(snapshot.created_at)}</span>
                </div>
              </article>
            ))
          ) : (
            <EmptyState
              title="No completed snapshots yet"
              description="Run and stop the world once, or create a manual backup while it is online."
            />
          )}
        </Surface>

        <aside className="sticky top-[30px] grid gap-[18px] max-[860px]:static max-[860px]:grid-cols-2 max-[620px]:grid-cols-1">
          <section className="rounded-md bg-[#dfe7d2] p-6 shadow-[inset_0_0_0_1px_rgba(70,99,51,0.14)]">
            <h2 className="m-0 font-display text-xl leading-[1.2] font-[800]">
              Capture now
            </h2>
            <p className="mt-2 mb-0 text-[13px] leading-normal text-[#526046]">
              Create a retained manual snapshot of the live world using the
              existing snapshot operation machinery.
            </p>
            <Button
              className="mt-[18px] w-full"
              disabled={!allowed.canSnapshot || manual.isPending}
              onClick={() => manual.mutate()}
            >
              <Camera size={16} />
              {manual.isPending ? "Requesting…" : "Create manual backup"}
            </Button>
            {!allowed.canSnapshot ? (
              <p className="mt-2 mb-0 text-[13px] leading-normal text-[#526046]">
                The runtime must be online for a manual backup.
              </p>
            ) : null}
          </section>

          <Surface className="p-[22px]">
            <h2 className="m-0 font-display text-xl leading-[1.2] font-[800]">
              Restore safety
            </h2>
            <ul className="mt-3.5 mb-0 pl-5 text-[13px] leading-[1.55] text-ink-soft">
              <li>Only retained, completed snapshots are listed.</li>
              <li>Restore is available while sleeping.</li>
              <li>A restore creates a new monotonic authoritative generation.</li>
            </ul>
          </Surface>
        </aside>
      </div>
    </>
  );
}
