import { AdminServersPage } from "../../../components/admin/servers";
import { isServerFilter } from "../../../components/admin/server-filters";

export default async function AdminServers({ searchParams }: { searchParams: Promise<{ filter?: string }> }) {
  const { filter } = await searchParams;
  return <AdminServersPage initialFilter={isServerFilter(filter) ? filter : "all"} />;
}
