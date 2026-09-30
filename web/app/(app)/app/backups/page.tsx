import { redirect } from "next/navigation";
import { Backups } from "../../../../components/backups";
import { getServer, getSnapshots } from "../../../../lib/api/server";

export default async function BackupsPage() {
  const server = await getServer();
  if (!server) redirect("/app/setup");
  const snapshots = await getSnapshots();
  return <Backups initialServer={server} initialSnapshots={snapshots} />;
}
