import { controlPlaneOrigin } from "../../../../../lib/control-plane";

export const dynamic = "force-dynamic";

export async function GET(request: Request) {
  const requestUrl = new URL(request.url);
  const upstreamUrl = new URL("/api/v1/server/events", controlPlaneOrigin());
  upstreamUrl.search = requestUrl.search;

  const headers = new Headers({ accept: "text/event-stream" });
  const cookie = request.headers.get("cookie");
  const lastEventId = request.headers.get("last-event-id");
  if (cookie) headers.set("cookie", cookie);
  if (lastEventId) headers.set("last-event-id", lastEventId);

  const upstream = await fetch(upstreamUrl, {
    headers,
    cache: "no-store",
    signal: request.signal,
  });
  const responseHeaders = new Headers({
    "cache-control": "no-cache, no-transform",
    "x-accel-buffering": "no",
    "x-content-type-options": "nosniff",
  });
  const contentType = upstream.headers.get("content-type");
  const setCookie = upstream.headers.get("set-cookie");
  if (contentType) responseHeaders.set("content-type", contentType);
  if (setCookie) responseHeaders.append("set-cookie", setCookie);

  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers: responseHeaders,
  });
}
