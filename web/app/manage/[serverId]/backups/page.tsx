import { Backups } from "../../../../components/backups";
import { getServer, getSnapshots } from "../../../../lib/api/server";
import { serverAccessPage } from "../server-access";

export default async function ServerAccessBackups({ params }: { params: Promise<{ serverId: string }> }) {
  return serverAccessPage({ params }, async (serverId) => {
    const [server, snapshots] = await Promise.all([getServer(serverId), getSnapshots(serverId)]);
    return <Backups initialServer={server} initialSnapshots={snapshots} />;
  });
}
