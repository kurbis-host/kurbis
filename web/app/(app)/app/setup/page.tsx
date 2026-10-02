import { redirect } from "next/navigation";
import { Setup } from "../../../../components/setup";
import { getAccount, getServers } from "../../../../lib/api/server";

export default async function SetupPage() {
  const [servers, account] = await Promise.all([getServers(), getAccount()]);
  const ownedWorldIds = new Set(account.worlds.map((world) => world.id));
  const ownedServers = servers.filter((server) => ownedWorldIds.has(server.id));
  const selected = ownedServers.find((server) => server.is_default) ?? ownedServers[0];
  if (selected) redirect(`/manage/${selected.id}`);
  return <Setup />;
}
