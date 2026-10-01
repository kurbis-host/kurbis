import { redirect } from "next/navigation";
import { getServers } from "../../../lib/api/server";

export default async function AppIndexPage() {
  const servers = await getServers();
  const selected = servers.find((server) => server.is_default) ?? servers[0];
  redirect(selected ? `/app/servers/${selected.id}` : "/app/setup");
}
