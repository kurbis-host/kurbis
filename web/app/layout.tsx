import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Melon — Minecraft hosting",
  description: "A small, fast control panel for your Pumpkin server.",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
