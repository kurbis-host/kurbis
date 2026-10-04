import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
	title: {
		default: "kurbis — Minecraft hosting that sleeps",
		template: "%s · kurbis",
	},
	description: "A Minecraft server that wakes when friends join, sleeps when they leave, and snapshots the world in between.",
	icons: {
		icon: "/kurbis.svg",
		shortcut: "/kurbis.svg",
		apple: "/kurbis.svg",
	},
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
