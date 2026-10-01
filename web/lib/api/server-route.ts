import { redirect } from "next/navigation";
import { ServerApiError, getServer, getServers } from "./server";

export async function getServerOrFallback(serverId: string, suffix = "") {
  try {
    return await getServer(serverId);
  } catch (error) {
    if (!(error instanceof ServerApiError) || error.status !== 404) throw error;

    const servers = await getServers();
    const selected = servers.find((server) => server.is_default) ?? servers[0];
    redirect(selected ? `/app/servers/${selected.id}${suffix}` : "/app/setup");
  }
}
