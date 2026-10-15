import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
	title: {
		default: "kurbis — free Minecraft servers for you and your friends",
		template: "%s · kurbis",
	},
	description: "A free Minecraft server that starts when friends join, stops when they leave, and saves the world in between.",
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
