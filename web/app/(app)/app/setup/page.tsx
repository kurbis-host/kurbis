import { redirect } from "next/navigation";
import { ClaimServer } from "../../../../components/claim-server";
import { getAccount, getSession } from "../../../../lib/api/server";

export default async function SetupPage() {
  const [session, account] = await Promise.all([getSession(), getAccount()]);
  const world = account.worlds.find((item) => item.is_default) ?? account.worlds[0];
  if (world) redirect(`/manage/${world.id}`);
  return <ClaimServer serverDomain={session.server_domain} />;
}
