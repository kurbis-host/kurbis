import { redirect } from "next/navigation";
import type { ReactNode } from "react";
import { AppProviders } from "../../../components/providers";
import { AppShell } from "../../../components/app-shell";
import { getServer, getSession, ServerApiError } from "../../../lib/api/server";

export default async function PanelLayout({ children }: { children: ReactNode }) {
  let session;
  try {
    session = await getSession();
  } catch (error) {
    if (error instanceof ServerApiError && error.status === 401) redirect("/");
    throw error;
  }
  const server = await getServer();
  return <AppProviders session={session} server={server}><AppShell>{children}</AppShell></AppProviders>;
}
