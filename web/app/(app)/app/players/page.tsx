import { redirect } from "next/navigation";
import { Players } from "../../../../components/players";
import { getPlayers, getServer, getWhitelist } from "../../../../lib/api/server";

export default async function PlayersPage() {
  const server = await getServer();
  if (!server) redirect("/app/setup");
  const [players, whitelist] = await Promise.all([getPlayers(), getWhitelist()]);
  return <Players initialServer={server} initialPlayers={players} initialWhitelist={whitelist} />;
}
