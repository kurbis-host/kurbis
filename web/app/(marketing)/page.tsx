import type { Metadata } from "next";
import { LandingPage } from "../../components/landing/landing-page";

export const metadata: Metadata = {
  title: { absolute: "kurbis — free Minecraft servers. Dig down." },
  description: "Free Minecraft servers for you and your friends. Java and Bedrock, one address, awake only while someone is playing.",
};

export default function MarketingPage() {
  return <LandingPage />;
}
