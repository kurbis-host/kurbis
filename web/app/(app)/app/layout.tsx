import { redirect } from "next/navigation";
import type { ReactNode } from "react";
import { AppProviders } from "../../../components/providers";
import { ServerPanelShell } from "../../../components/server-panel-shell";
import { getServers, getServerSessions, getSession, ServerApiError } from "../../../lib/api/server";

export default async function PanelLayout({ children }: { children: ReactNode }) {
  let session;
  try {
    session = await getSession();
  } catch (error) {
    if (error instanceof ServerApiError && error.status === 401) redirect("/");
    throw error;
  }
  const [servers, serverSessions] = await Promise.all([getServers(), getServerSessions()]);
  return <AppProviders session={session} servers={servers} serverSessions={serverSessions}>
    <ServerPanelShell session={session} servers={servers} serverSessions={serverSessions}>{children}</ServerPanelShell>
  </AppProviders>;
}
