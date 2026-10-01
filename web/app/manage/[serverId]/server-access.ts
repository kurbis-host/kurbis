import { notFound, redirect } from "next/navigation";
import { ServerApiError } from "../../../lib/api/server";

type ServerParams = { params: Promise<{ serverId: string }> };

export async function withServerAccess<T>(load: () => Promise<T>): Promise<T> {
  try {
    return await load();
  } catch (error) {
    if (error instanceof ServerApiError && error.status === 401) {
      redirect("/manage/access-expired");
    }
    if (error instanceof ServerApiError && error.status === 404) notFound();
    throw error;
  }
}

export async function serverAccessPage<T>(
  { params }: ServerParams,
  render: (serverId: string) => Promise<T>,
): Promise<T> {
  const { serverId } = await params;
  return withServerAccess(() => render(serverId));
}
