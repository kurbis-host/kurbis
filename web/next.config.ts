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
  "/auth/logout",
];

const nextConfig: NextConfig = {
  distDir: process.env.NEXT_DIST_DIR ?? ".next",
  poweredByHeader: false,
  reactStrictMode: true,
  async rewrites() {
    return [
      { source: "/api/:path*", destination: `${controlPlane}/api/:path*` },
      ...authPaths.map((source) => ({ source, destination: `${controlPlane}${source}` })),
    ];
  },
};

export default nextConfig;
