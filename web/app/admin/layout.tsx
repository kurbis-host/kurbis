import type { Metadata } from "next";
import { notFound } from "next/navigation";
import type { ReactNode } from "react";
import { AdminShell } from "../../components/admin/shell";
import { AdminProviders } from "../../components/providers";
import { getSessionOrNull } from "../../lib/api/server";

export const metadata: Metadata = {
  title: "Admin · Kurbis",
  robots: { index: false, follow: false },
};

/** The operator dashboard. To anyone but an admin it does not exist, as its API does not. */
export default async function AdminLayout({ children }: { children: ReactNode }) {
  const session = await getSessionOrNull();
  if (!session?.is_admin) notFound();
  return <AdminProviders><AdminShell session={session}>{children}</AdminShell></AdminProviders>;
}
