import { Players } from "../../../../components/players";
import { getPlayers, getServer, getWhitelist } from "../../../../lib/api/server";
import { serverAccessPage } from "../server-access";

export default async function ServerAccessPlayers({ params }: { params: Promise<{ serverId: string }> }) {
  return serverAccessPage({ params }, async (serverId) => {
    const [server, players, whitelist] = await Promise.all([getServer(serverId), getPlayers(serverId), getWhitelist(serverId)]);
    return <Players initialServer={server} initialPlayers={players} initialWhitelist={whitelist} />;
  });
}
