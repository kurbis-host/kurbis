import http from "node:http";
import https from "node:https";
import next from "next";

const CONTROL_PLANE_AUTH_PATHS = new Set([
  "/auth/google/login",
  "/auth/google/link",
  "/auth/google/callback",
  "/auth/discord/login",
  "/auth/discord/link",
  "/auth/discord/callback",
  "/auth/minecraft/redeem",
  "/auth/server/redeem",
  "/auth/logout",
]);

function parseArguments(argv) {
  let dev = false;
  let hostname = process.env.HOSTNAME || "0.0.0.0";
  let port = Number(process.env.PORT || "3000");

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--dev") {
      dev = true;
      continue;
    }
    if (argument === "--hostname" || argument === "-H") {
      hostname = argv[++index];
      continue;
    }
    if (argument === "--port" || argument === "-p") {
      port = Number(argv[++index]);
      continue;
    }
    throw new Error(`unsupported panel server argument: ${argument}`);
  }

  if (!hostname) throw new Error("panel hostname must not be empty");
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`panel port must be an integer between 1 and 65535; received ${port}`);
  }
  return { dev, hostname, port };
}

function controlPlaneUrl() {
  const configured = process.env.CONTROL_PLANE_INTERNAL_URL?.trim() || "http://127.0.0.1:8080";
  const target = new URL(configured);
  if (target.protocol !== "http:" && target.protocol !== "https:") {
    throw new Error("CONTROL_PLANE_INTERNAL_URL must use http or https");
  }
  if (target.pathname !== "/" || target.search || target.hash) {
    throw new Error("CONTROL_PLANE_INTERNAL_URL must contain only an origin");
  }
  return target;
}

function shouldProxy(pathname) {
  return pathname.startsWith("/api/") || CONTROL_PLANE_AUTH_PATHS.has(pathname);
}

function proxyRequest(request, response, target) {
  const requestUrl = new URL(request.url || "/", "http://panel.invalid");
  const upstreamUrl = new URL(`${requestUrl.pathname}${requestUrl.search}`, target);
  const headers = { ...request.headers, host: upstreamUrl.host };
  delete headers.connection;
  delete headers["proxy-connection"];
  delete headers.upgrade;

  const transport = upstreamUrl.protocol === "https:" ? https : http;
  const upstream = transport.request(upstreamUrl, {
    method: request.method,
    headers,
  }, (upstreamResponse) => {
    const responseHeaders = { ...upstreamResponse.headers };
    delete responseHeaders.connection;
    delete responseHeaders["transfer-encoding"];
    response.writeHead(upstreamResponse.statusCode || 502, upstreamResponse.statusMessage, responseHeaders);
    upstreamResponse.pipe(response);
  });

  upstream.on("error", (error) => {
    if (response.headersSent) {
      response.destroy(error);
      return;
    }
    response.writeHead(502, { "content-type": "text/plain; charset=utf-8" });
    response.end("control plane unavailable");
  });
  response.on("close", () => {
    if (!response.writableEnded) upstream.destroy();
  });
  request.pipe(upstream);
}

const { dev, hostname, port } = parseArguments(process.argv.slice(2));
const target = controlPlaneUrl();
const app = next({ dev, hostname, port });
const handle = app.getRequestHandler();

await app.prepare();

const server = http.createServer((request, response) => {
  let requestUrl;
  try {
    requestUrl = new URL(request.url || "/", "http://panel.invalid");
  } catch {
    response.writeHead(400, { "content-type": "text/plain; charset=utf-8" });
    response.end("invalid request URL");
    return;
  }

  if (shouldProxy(requestUrl.pathname)) {
    proxyRequest(request, response, target);
    return;
  }
  void handle(request, response);
});

server.listen(port, hostname, () => {
  console.log(`Kurbis panel listening on http://${hostname}:${port}`);
});
