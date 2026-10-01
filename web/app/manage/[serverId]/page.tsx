import { Overview } from "../../../components/overview";
import { getActivity, getServer } from "../../../lib/api/server";
import { serverAccessPage } from "./server-access";

export default async function ServerAccessOverview({ params }: { params: Promise<{ serverId: string }> }) {
  return serverAccessPage({ params }, async (serverId) => {
    const [server, activity] = await Promise.all([getServer(serverId), getActivity(serverId)]);
    return <Overview initialServer={server} initialActivity={activity} renderedAt={Date.now()} panelBasePath={`/manage/${server.id}`} />;
  });
}
