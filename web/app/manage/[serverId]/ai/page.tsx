import { AiAssistant } from "../../../../components/ai";
import { getAiOverview, getServer } from "../../../../lib/api/server";
import { serverAccessPage } from "../server-access";

export default async function ServerAccessAi({ params }: { params: Promise<{ serverId: string }> }) {
  return serverAccessPage({ params }, async (serverId) => {
    const [server, overview] = await Promise.all([getServer(serverId), getAiOverview(serverId)]);
    return <AiAssistant serverId={server.id} initialOverview={overview} renderedAt={Date.now()} />;
  });
}
