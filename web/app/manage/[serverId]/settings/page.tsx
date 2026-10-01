import { Settings } from "../../../../components/settings";
import { getServer } from "../../../../lib/api/server";
import { serverAccessPage } from "../server-access";

export default async function ServerAccessSettings({ params }: { params: Promise<{ serverId: string }> }) {
  return serverAccessPage({ params }, async (serverId) => {
    return <Settings initialServer={await getServer(serverId)} afterDeleteHref="/manage/server-removed" />;
  });
}
