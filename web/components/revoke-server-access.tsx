"use client";

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ShieldOff } from "lucide-react";
import { useState } from "react";
import { errorMessage } from "../lib/api/client";
import { browserApi } from "../lib/api/operations";
import { queryKeys } from "../lib/query";
import { Button } from "./ui";

export function RevokeServerAccess({
  sessionId,
  onRevoked,
}: {
  sessionId: string;
  onRevoked?: () => void;
}) {
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => browserApi.revokeServerSession(sessionId),
    onMutate: () => setError(null),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.serverSessions }),
        queryClient.invalidateQueries({ queryKey: queryKeys.servers }),
      ]);
      onRevoked?.();
    },
    onError: (reason) => setError(errorMessage(reason)),
  });

  return (
    <span className="grid shrink-0 justify-items-end gap-1">
      <Button size="small" variant="danger" disabled={mutation.isPending} onClick={() => mutation.mutate()}>
        <ShieldOff size={14} />
        {mutation.isPending ? "Revoking…" : "Revoke access"}
      </Button>
      {error ? <span className="max-w-48 text-right text-[11px] text-[#9b3730]" role="alert">{error}</span> : null}
    </span>
  );
}
