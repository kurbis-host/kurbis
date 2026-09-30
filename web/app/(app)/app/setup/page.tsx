import { redirect } from "next/navigation";
import { Setup } from "../../../../components/setup";
import { getServer } from "../../../../lib/api/server";

export default async function SetupPage() {
  if (await getServer()) redirect("/app");
  return <Setup />;
}
