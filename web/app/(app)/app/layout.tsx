import { redirect } from "next/navigation";
import type { ReactNode } from "react";
import { AppProviders } from "../../../components/providers";
import { AppShell } from "../../../components/app-shell";
import { getServers, getSession, ServerApiError } from "../../../lib/api/server";

export default async function PanelLayout({ children }: { children: ReactNode }) {
  let session;
  try {
    session = await getSession();
  } catch (error) {
    if (error instanceof ServerApiError && error.status === 401) redirect("/");
    throw error;
  }
  const servers = await getServers();
  return <AppProviders session={session} servers={servers}><AppShell servers={servers}>{children}</AppShell></AppProviders>;
}
