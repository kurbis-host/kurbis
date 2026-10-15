import { redirect } from "next/navigation";
import { ServerList } from "../../../../components/server-list";
import { getAccount, getServerSessions, getServers } from "../../../../lib/api/server";

export default async function ServersPage() {
  const [servers, sessions, account] = await Promise.all([getServers(), getServerSessions(), getAccount()]);
  // Nothing to list yet (a new account, as after signing in from the landing page): claim a server.
  if (servers.length === 0 && account.worlds.length === 0) redirect("/app/setup");
  return <ServerList servers={servers} sessions={sessions} canClaimServer={account.worlds.length === 0} />;
}
