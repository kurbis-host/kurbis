import type { Metadata } from "next";
import "./globals.css";

const title = "kurbis — free Minecraft servers for you and your friends";
const description = "A free Minecraft server that starts when friends join, stops when they leave, and saves the world in between.";
const banner = { url: "/kurbis-banner.png", width: 2560, height: 1280, alt: "kurbis — free Minecraft servers" };

export const metadata: Metadata = {
	metadataBase: new URL("https://kurbis.host"),
	title: {
		default: title,
		template: "%s · kurbis",
	},
	description,
	openGraph: { type: "website", siteName: "kurbis", title, description, images: [banner] },
	twitter: { card: "summary_large_image", title, description, images: [banner] },
	// The icons are files beside this one: favicon.ico, icon.svg and apple-icon.png (a PNG, as iOS wants for the home screen).
};

export default function RootLayout({
	children,
}: Readonly<{
	children: React.ReactNode;
}>) {
	return (
		<html lang="en" data-scroll-behavior="smooth">
			<body>{children}</body>
		</html>
	);
}
