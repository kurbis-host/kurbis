import { Overview } from "../../../../../components/overview";
import { getActivity } from "../../../../../lib/api/server";
import { getServerOrFallback } from "../../../../../lib/api/server-route";

export default async function OverviewPage({ params }: { params: Promise<{ serverId: string }> }) {
  const { serverId } = await params;
  const server = await getServerOrFallback(serverId);
  const activity = await getActivity(serverId);
  return <Overview initialServer={server} initialActivity={activity} renderedAt={Date.now()} />;
}
