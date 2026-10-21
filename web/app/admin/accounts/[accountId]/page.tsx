import { AdminAccountPage } from "../../../../components/admin/accounts";

export default async function AdminAccount({ params }: { params: Promise<{ accountId: string }> }) {
  const { accountId } = await params;
  return <AdminAccountPage accountId={accountId} />;
}
