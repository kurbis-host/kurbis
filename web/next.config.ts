import type { NextConfig } from "next";
import { controlPlaneOrigin } from "./lib/control-plane";

const controlPlane = controlPlaneOrigin();
const authPaths = [
  "/auth/google/login",
  "/auth/google/link",
  "/auth/google/callback",
  "/auth/discord/login",
  "/auth/discord/link",
  "/auth/discord/callback",
  "/auth/minecraft/redeem",
  "/auth/server/redeem",
  "/auth/logout",
];
const isProduction = process.env.NODE_ENV === "production";
const securityHeaders = [
  { key: "Content-Security-Policy", value: [
    "default-src 'self'",
    "base-uri 'self'",
    "object-src 'none'",
    "frame-ancestors 'none'",
    "form-action 'self'",
    "img-src 'self' data:",
    "font-src 'self' data:",
    "style-src 'self' 'unsafe-inline'",
    `script-src 'self' 'unsafe-inline'${isProduction ? "" : " 'unsafe-eval'"}`,
    `connect-src 'self'${isProduction ? "" : " ws:"}`,
  ].join("; ") },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "X-Frame-Options", value: "DENY" },
  { key: "Permissions-Policy", value: "camera=(), microphone=(), geolocation=()" },
  ...(isProduction
    ? [{ key: "Strict-Transport-Security", value: "max-age=31536000" }]
    : []),
];

const nextConfig: NextConfig = {
  distDir: process.env.NEXT_DIST_DIR ?? ".next",
  poweredByHeader: false,
  reactStrictMode: true,
  async headers() {
    return [{ source: "/:path*", headers: securityHeaders }];
  },
  async rewrites() {
    return [
      { source: "/api/:path*", destination: `${controlPlane}/api/:path*` },
      ...authPaths.map((source) => ({ source, destination: `${controlPlane}${source}` })),
    ];
  },
};

export default nextConfig;
