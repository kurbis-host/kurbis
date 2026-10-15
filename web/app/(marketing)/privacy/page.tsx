import type { Metadata } from "next";
import { LegalPage } from "../../../components/legal-page";

export const metadata: Metadata = {
	title: "Privacy",
	description: "What Kurbis knows about you, why, for how long, and what you can do about it.",
};

export default function PrivacyPage() {
	return (
		<LegalPage
			title="Privacy"
			lead="What Kurbis knows about you, why, for how long, and what you can do about it."
			updated="14 October 2026"
		>
			<h2>Who is responsible</h2>
			<p>
				Samuel Scheit, Keplerstrasse 23, 85609 Aschheim, Germany, info@kurbis.host. See also the <a href="/impressum">Impressum</a>.
			</p>

			<h2>Visiting the website</h2>
			<p>
				To send you this website, our servers process your IP address, the time of the request and what your browser tells them
				about itself (Art. 6 (1) (f) GDPR: we cannot deliver the page otherwise). The site runs at HOSTING PROVIDER, COUNTRY.
				Technical logs are deleted after LOG RETENTION.
			</p>
			<p>
				There are no analytics, no ads and no tracking cookies. The landing page and its 3D world come from our own server; nothing
				on it loads from anyone else.
			</p>
			<p>
				When you type a name for your server, it is sent to us to check whether it is free; the check stores nothing. Your browser
				keeps the name in its local storage, so the setup page can offer it after you sign in and the landing page has it again when
				you come back. It stays there until you clear this site&apos;s data in your browser.
			</p>

			<h2>Signing in</h2>
			<p>
				If you sign in with Google or Discord, they send us your account ID with them, your display name, your email address, and
				whether it is verified. We use these to run your account and, where both confirm the same verified email, to link the two
				sign-ins (Art. 6 (1) (b) GDPR).
			</p>
			<ul>
				<li>
					<code>kurbis_session</code> keeps you signed in. It is strictly necessary, lasts 30 days, and is renewed while you use
					the panel.
				</li>
				<li>A short-lived cookie (10 minutes) protects the sign-in itself while you are away at Google or Discord.</li>
			</ul>

			<h2>Joining from Minecraft</h2>
			<p>
				When you join a Kurbis server, your Minecraft account is verified with Mojang and Microsoft (Java Edition) or with Xbox
				(Bedrock Edition). We store your player ID and name. The first time you join an address nobody has taken, this creates your
				account and a server at that address (Art. 6 (1) (b) GDPR).
			</p>

			<h2>Your server</h2>
			<p>Everything that makes up your server is kept for as long as the server exists, and deleted with it:</p>
			<ul>
				<li>its world, settings and backups (older backups are removed automatically);</li>
				<li>its whitelist and operators: player names and IDs;</li>
				<li>who played on it: player name, edition, and when they connected and were last seen;</li>
				<li>its logs and activity, which can include player names, chat and the commands people type.</li>
			</ul>
			<p>You can download your world at any time from the panel.</p>

			<h2>The AI assistant</h2>
			<p>
				If an operator of a server uses <code>/ai</code>, what they ask, and what of the server it needs to answer, is sent to{" "}
				Anthropic Inc., USA to produce the answer (Art. 6 (1) (b) GDPR). Conversations are deleted after 30 days; usage records,
				kept to hold each server to its budget, after 400 days.
			</p>

			<h2>Plugins</h2>
			<p>
				The panel&apos;s plugin market shows icons from <code>assets.pumpkinmc.org</code>, the Pumpkin plugin market, so that host
				sees your IP address when you open the market.
			</p>

			<h2>Who else receives data</h2>
			<p>
				Our hosting provider; Google or Discord, if you sign in with them; Mojang, Microsoft and Xbox, to verify players; the AI
				provider, for <code>/ai</code>; and the plugin market, for its icons. We sell nothing to anyone.
			</p>

			<h2>Your rights</h2>
			<p>
				You can ask what we store about you, and have it corrected, deleted, restricted or handed to you (Art. 15 to 20 GDPR). You
				can object to processing based on our legitimate interests (Art. 21 GDPR). To do any of this, or to delete your account,
				write to info@kurbis.host. You may also complain to a data protection authority, for example Bavarian State Office for Data
				Protection Supervision (Bayerisches Landesamt für Datenschutzaufsicht - BayLDA). .
			</p>
		</LegalPage>
	);
}
