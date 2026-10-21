import { notFound } from "next/navigation";
import { ServerLifecycle } from "../../../../components/admin/lifecycle";
import { getSessionOrNull } from "../../../../lib/api/server";

/** Admin only: for anyone else the page does not exist. */
export default async function ServerLifecyclePage({ params }: { params: Promise<{ serverId: string }> }) {
  const [{ serverId }, session] = await Promise.all([params, getSessionOrNull()]);
  if (!session?.is_admin) notFound();
  return <ServerLifecycle serverId={serverId} />;
}
