"use client";

import { RefreshCw, TriangleAlert } from "lucide-react";
import { Button, Notice, PageHeader } from "../../../components/ui";

export default function AppError({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  return <div><PageHeader title="The panel hit a snag" description="The control plane did not return the data this page needs." /><Notice tone="danger"><strong>{error.message}</strong>{error.digest ? <div>Reference: {error.digest}</div> : null}</Notice><div className="mt-[18px]"><Button onClick={reset}><RefreshCw size={17} />Try again</Button></div></div>;
}
