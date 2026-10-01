import { Console } from "../../../../../../components/console";
import { getLogs } from "../../../../../../lib/api/server";
import { getServerOrFallback } from "../../../../../../lib/api/server-route";

export default async function ConsolePage({ params }: { params: Promise<{ serverId: string }> }) {
  const { serverId } = await params;
  const server = await getServerOrFallback(serverId, "/console");
  const logs = await getLogs(serverId);
  return <Console serverId={server.id} initialLogs={logs} />;
}
