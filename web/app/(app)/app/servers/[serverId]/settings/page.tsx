import { Settings } from "../../../../../../components/settings";
import { getServerOrFallback } from "../../../../../../lib/api/server-route";

export default async function SettingsPage({ params }: { params: Promise<{ serverId: string }> }) {
  const { serverId } = await params;
  return <Settings initialServer={await getServerOrFallback(serverId, "/settings")} />;
}
