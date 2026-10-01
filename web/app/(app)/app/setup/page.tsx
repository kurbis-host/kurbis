import { redirect } from "next/navigation";
import { Setup } from "../../../../components/setup";
import { getServers } from "../../../../lib/api/server";

export default async function SetupPage() {
  const servers = await getServers();
  const selected = servers.find((server) => server.is_default) ?? servers[0];
  if (selected) redirect(`/app/servers/${selected.id}`);
  return <Setup />;
}
