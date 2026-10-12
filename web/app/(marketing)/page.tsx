import type { Metadata } from "next";
import { Landing } from "../../components/landing/landing";
import { Mirror } from "../../components/landing/mirror";
import { bootScript, workerUrl } from "../../lib/landing/boot";

export const metadata: Metadata = {
  title: { absolute: "kurbis — free Minecraft servers. Dig down." },
  description: "Free Minecraft servers for you and your friends. Java and Bedrock, one address, awake only while someone is playing.",
};

export default function MarketingPage() {
  const worker = workerUrl();
  return (
    <>
      {/* The film's world is being built, and its files fetched, from here on: before the page's own code is even asked for. */}
      <script dangerouslySetInnerHTML={{ __html: bootScript(worker) }} />
      <Landing worker={worker}>
        <Mirror />
      </Landing>
    </>
  );
}
