import { redirect } from "next/navigation";
import { Console } from "../../../../components/console";
import { getLogs, getServer } from "../../../../lib/api/server";

export default async function ConsolePage() {
  const server = await getServer();
  if (!server) redirect("/app/setup");
  const logs = await getLogs();
  return <Console initialLogs={logs} />;
}
