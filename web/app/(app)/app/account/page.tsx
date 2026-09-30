import { Account } from "../../../../components/account";
import { getAccount, getServer, getSession } from "../../../../lib/api/server";

export default async function AccountPage() {
  const [session, server, account] = await Promise.all([getSession(), getServer(), getAccount()]);
  return <Account initialSession={session} initialServer={server} initialAccount={account} />;
}
