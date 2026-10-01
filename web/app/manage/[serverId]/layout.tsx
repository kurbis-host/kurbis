import type { ReactNode } from "react";
import { ServerAccessShell } from "../../../components/server-access-shell";
import { ServerProviders } from "../../../components/providers";
import { getServer } from "../../../lib/api/server";
import { withServerAccess } from "./server-access";

export default async function ServerAccessLayout({ children, params }: { children: ReactNode; params: Promise<{ serverId: string }> }) {
  return withServerAccess(async () => {
    const { serverId } = await params;
    const server = await getServer(serverId);
    return <ServerProviders server={server}><ServerAccessShell server={server}>{children}</ServerAccessShell></ServerProviders>;
  });
}
