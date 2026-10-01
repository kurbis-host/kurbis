import { Console } from "../../../../components/console";
import { getLogs, getServer } from "../../../../lib/api/server";
import { serverAccessPage } from "../server-access";

export default async function ServerAccessConsole({ params }: { params: Promise<{ serverId: string }> }) {
  return serverAccessPage({ params }, async (serverId) => {
    const [server, logs] = await Promise.all([getServer(serverId), getLogs(serverId)]);
    return <Console serverId={server.id} initialLogs={logs} />;
  });
}
