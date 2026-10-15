import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
	title: {
		default: "kurbis — free Minecraft servers for you and your friends",
		template: "%s · kurbis",
	},
	description: "A free Minecraft server that starts when friends join, stops when they leave, and saves the world in between.",
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
