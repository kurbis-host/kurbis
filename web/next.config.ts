import type { NextConfig } from "next";
const isProduction = process.env.NODE_ENV === "production";
const securityHeaders = [
	{
		key: "Content-Security-Policy",
		value: [
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
		].join("; "),
	},
	{ key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
	{ key: "X-Content-Type-Options", value: "nosniff" },
	{ key: "X-Frame-Options", value: "DENY" },
	{ key: "Permissions-Policy", value: "camera=(), microphone=(), geolocation=()" },
	...(isProduction ? [{ key: "Strict-Transport-Security", value: "max-age=31536000" }] : []),
];

const nextConfig: NextConfig = {
	distDir: process.env.NEXT_DIST_DIR ?? ".next",
	poweredByHeader: false,
	reactStrictMode: true,
	async headers() {
		return [
			{ source: "/:path*", headers: securityHeaders },
			// The landing film's bundle. The page asks for each of its files by the bundle's
			// version (lib/landing/engine/address.ts), so what an address names never changes
			// and a browser may keep it for good. Without a version it is revalidated as ever.
			{
				source: "/landing/:path*",
				has: [{ type: "query", key: "v" }],
				headers: [{ key: "Cache-Control", value: "public, max-age=31536000, immutable" }],
			},
			// What is not text in the bundle is stored through gzip (once, as hard as gzip
			// packs) and sent as it is: this is what tells the browser, which then unpacks
			// it on the way in. Neither the server nor the CDN would pack such bytes itself.
			// (The type is said as well: named gzip there too, some browsers leave a file packed.)
			{
				source: "/landing/:file(.*\\.gz)",
				headers: [
					{ key: "Content-Encoding", value: "gzip" },
					{ key: "Content-Type", value: "application/octet-stream" },
				],
			},
		];
	},
	allowedDevOrigins: ["127.0.0.1", "localhost"],
};

export default nextConfig;
