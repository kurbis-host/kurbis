import { ServerList } from "../../../../components/server-list";
import { getAccount, getServerSessions, getServers } from "../../../../lib/api/server";

export default async function ServersPage() {
  const [servers, sessions, account] = await Promise.all([getServers(), getServerSessions(), getAccount()]);
  return <ServerList servers={servers} sessions={sessions} canCreateServer={account.worlds.length === 0} />;
}
