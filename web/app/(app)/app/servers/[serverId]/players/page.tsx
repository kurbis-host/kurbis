import { Players } from "../../../../../../components/players";
import { getPlayers, getWhitelist } from "../../../../../../lib/api/server";
import { getServerOrFallback } from "../../../../../../lib/api/server-route";

export default async function PlayersPage({ params }: { params: Promise<{ serverId: string }> }) {
  const { serverId } = await params;
  const server = await getServerOrFallback(serverId, "/players");
  const [players, whitelist] = await Promise.all([getPlayers(serverId), getWhitelist(serverId)]);
  return <Players initialServer={server} initialPlayers={players} initialWhitelist={whitelist} />;
}
