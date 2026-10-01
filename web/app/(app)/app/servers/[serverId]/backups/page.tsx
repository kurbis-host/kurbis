import { Backups } from "../../../../../../components/backups";
import { getSnapshots } from "../../../../../../lib/api/server";
import { getServerOrFallback } from "../../../../../../lib/api/server-route";

export default async function BackupsPage({ params }: { params: Promise<{ serverId: string }> }) {
  const { serverId } = await params;
  const server = await getServerOrFallback(serverId, "/backups");
  const snapshots = await getSnapshots(serverId);
  return <Backups initialServer={server} initialSnapshots={snapshots} />;
}
