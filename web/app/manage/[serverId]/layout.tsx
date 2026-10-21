import type { ReactNode } from "react";
import { ServerPanelShell } from "../../../components/server-panel-shell";
import { ServerProviders } from "../../../components/providers";
import { getServer, getServers, getServerSessions, getSessionOrNull } from "../../../lib/api/server";
import { withServerAccess } from "./server-access";

export default async function ServerAccessLayout({ children, params }: { children: ReactNode; params: Promise<{ serverId: string }> }) {
  return withServerAccess(async () => {
    const { serverId } = await params;
    const [server, session, serverSessions] = await Promise.all([
      getServer(serverId),
      getSessionOrNull(),
      getServerSessions(),
    ]);
    const servers = await getServers();
    // An admin reaches every server, but only reads the ones that are not theirs.
    const adminView = session?.is_admin === true
      && !servers.some((item) => item.id === server.id)
      && !serverSessions.some((grant) => grant.server_id === server.id);
    return <ServerProviders server={server} session={session ?? undefined} servers={servers} serverSessions={serverSessions} readOnly={adminView}>
      <ServerPanelShell scoped={!session} adminView={adminView} server={server} session={session ?? undefined} servers={servers} serverSessions={serverSessions}>{children}</ServerPanelShell>
    </ServerProviders>;
  });
}
