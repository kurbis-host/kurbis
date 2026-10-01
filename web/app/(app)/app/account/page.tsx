import { Account } from "../../../../components/account";
import { getAccount, getServers, getSession } from "../../../../lib/api/server";

export default async function AccountPage() {
  const [session, servers, account] = await Promise.all([getSession(), getServers(), getAccount()]);
  return <Account initialSession={session} initialServers={servers} initialAccount={account} />;
}
