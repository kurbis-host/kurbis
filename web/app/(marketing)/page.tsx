import type { Metadata } from "next";
import { Landing } from "../../components/landing/landing";
import { Mirror } from "../../components/landing/mirror";

export const metadata: Metadata = {
  title: { absolute: "kurbis — free Minecraft servers. Dig down." },
  description: "Free Minecraft servers for you and your friends. Java and Bedrock, one address, awake only while someone is playing.",
};

export default function MarketingPage() {
  return (
    <Landing>
      <Mirror />
    </Landing>
  );
}
