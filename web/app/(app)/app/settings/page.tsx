import { redirect } from "next/navigation";
import { Settings } from "../../../../components/settings";
import { getServer } from "../../../../lib/api/server";

export default async function SettingsPage() {
  const server = await getServer();
  if (!server) redirect("/app/setup");
  return <Settings initialServer={server} />;
}
