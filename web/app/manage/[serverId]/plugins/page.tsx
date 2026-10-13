import { Plugins } from "../../../../components/plugins";
import { getPlugins, getServer } from "../../../../lib/api/server";
import { serverAccessPage } from "../server-access";

export default async function ServerAccessPlugins({ params }: { params: Promise<{ serverId: string }> }) {
  return serverAccessPage({ params }, async (serverId) => {
    const [server, plugins] = await Promise.all([getServer(serverId), getPlugins(serverId)]);
    return <Plugins initialServer={server} initialPlugins={plugins} />;
  });
}
