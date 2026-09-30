import { redirect } from "next/navigation";
import { Overview } from "../../../components/overview";
import { getActivity, getServer } from "../../../lib/api/server";

export default async function OverviewPage() {
  const server = await getServer();
  if (!server) redirect("/app/setup");
  const activity = await getActivity();
  return (
    <Overview
      initialServer={server}
      initialActivity={activity}
      renderedAt={Date.now()}
    />
  );
}
