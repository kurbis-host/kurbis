"use client";

import { FormEvent, useCallback, useEffect, useMemo, useState } from "react";
import { ApiError, LogLine, Server, Settings, User, api, apiUrl } from "../lib/api";

const STATE_COPY: Record<Server["observed_state"], string> = {
	sleeping: "Asleep",
	queued: "Queued",
	restoring: "Restoring world",
	starting: "Starting",
	online: "Online",
	stopping: "Stopping",
	snapshotting: "Saving world",
	failed: "Needs attention",
	deleted: "Deleted",
};

type NormalizedSettings = {
	motd: string;
	difficulty: NonNullable<Settings["difficulty"]>;
	gamemode: NonNullable<Settings["gamemode"]>;
	view_distance: number;
	simulation_distance: number;
	hardcore: boolean;
	whitelist: boolean;
};

const DEFAULT_SETTINGS: NormalizedSettings = {
	motd: "Hosted by kurbis",
	difficulty: "Normal",
	gamemode: "Survival",
	view_distance: 12,
	simulation_distance: 8,
	hardcore: false,
	whitelist: false,
};

function normalizeSettings(settings: Settings): NormalizedSettings {
	return {
		motd: settings.motd ?? DEFAULT_SETTINGS.motd,
		difficulty: settings.difficulty ?? DEFAULT_SETTINGS.difficulty,
		gamemode: settings.gamemode ?? DEFAULT_SETTINGS.gamemode,
		view_distance: settings.view_distance ?? DEFAULT_SETTINGS.view_distance,
		simulation_distance: settings.simulation_distance ?? DEFAULT_SETTINGS.simulation_distance,
		hardcore: settings.hardcore ?? DEFAULT_SETTINGS.hardcore,
		whitelist: settings.whitelist ?? DEFAULT_SETTINGS.whitelist,
	};
}

function formatBytes(bytes: number) {
	if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
	if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
	return `${(bytes / 1024 ** 3).toFixed(2)} GiB`;
}

export default function Home() {
	const [user, setUser] = useState<User | null>(null);
	const [server, setServer] = useState<Server | null>(null);
	const [logs, setLogs] = useState<LogLine[]>([]);
	const [loading, setLoading] = useState(true);
	const [authRequired, setAuthRequired] = useState(false);
	const [busy, setBusy] = useState<string | null>(null);
	const [error, setError] = useState<string | null>(null);

	const refresh = useCallback(async () => {
		try {
			const [nextUser, nextServer] = await Promise.all([api<User>("/api/me"), api<Server | null>("/api/server")]);
			setUser(nextUser);
			setServer(nextServer);
			setAuthRequired(false);
		} catch (reason) {
			if (reason instanceof ApiError && reason.status === 401) {
				setAuthRequired(true);
				setUser(null);
				setServer(null);
			} else {
				setError(reason instanceof Error ? reason.message : "Request failed");
			}
		} finally {
			setLoading(false);
		}
	}, []);

	const refreshLogs = useCallback(async () => {
		if (!server) return;
		try {
			setLogs(await api<LogLine[]>("/api/server/logs?limit=160"));
		} catch {
			// Logs are supplementary; the state poll remains authoritative.
		}
	}, [server]);

	useEffect(() => {
		void refresh();
		const interval = window.setInterval(() => void refresh(), 2500);
		return () => window.clearInterval(interval);
	}, [refresh]);

	useEffect(() => {
		if (!server) {
			setLogs([]);
			return;
		}
		void refreshLogs();
		const interval = window.setInterval(() => void refreshLogs(), 4000);
		return () => window.clearInterval(interval);
	}, [server?.id, refreshLogs]);

	async function action(name: "start" | "stop" | "restart") {
		setBusy(name);
		setError(null);
		try {
			await api(`/api/server/${name}`, { method: "POST" });
			await refresh();
		} catch (reason) {
			setError(reason instanceof Error ? reason.message : "Action failed");
		} finally {
			setBusy(null);
		}
	}

	if (loading) {
		return <BootScreen />;
	}

	if (authRequired) {
		return <LoginScreen />;
	}

	return (
		<main className="shell">
			<header className="topbar">
				<a className="brand" href="/">
					<span className="brandMark" aria-hidden="true">
						M
					</span>
					<span>kurbis</span>
				</a>
				<div className="account">
					<span className="accountName">{user?.display_name}</span>
					<button
						className="textButton"
						onClick={async () => {
							await api("/auth/logout", { method: "POST" });
							setAuthRequired(true);
						}}
					>
						Sign out
					</button>
				</div>
			</header>

			<section className="hero">
				<p className="eyebrow">PUMPKIN CLOUD / SINGLE SERVER</p>
				<h1>{server ? "Your world, without the server chores." : "Plant a server."}</h1>
				<p className="lede">
					{server
						? "Two gigabytes of memory, five gigabytes of world storage, automatic sleep, and one address to remember."
						: "Claim a subdomain. We handle the runtime, snapshots, sleeping, and wake-up path."}
				</p>
			</section>

			{error && (
				<div className="alert" role="alert">
					<span>!</span>
					<div>
						<strong>Couldn’t finish that request.</strong>
						<p>{error}</p>
					</div>
					<button aria-label="Dismiss error" onClick={() => setError(null)}>
						×
					</button>
				</div>
			)}

			{server ? (
				<Dashboard server={server} logs={logs} busy={busy} onAction={action} onChanged={refresh} onError={setError} />
			) : (
				<CreateServer onCreated={refresh} onError={setError} />
			)}

			<footer>
				<span>kurbis v1</span>
				<span className="footerDot" />
				<span>Pumpkin-powered</span>
			</footer>
		</main>
	);
}

function BootScreen() {
	return (
		<main className="centerStage" aria-live="polite">
			<div className="bootMark">M</div>
			<p className="eyebrow">OPENING CONTROL PANEL</p>
			<div className="loadingBar">
				<span />
			</div>
		</main>
	);
}

function LoginScreen() {
	return (
		<main className="loginLayout">
			<section className="loginCopy">
				<div className="brand brandLarge">
					<span className="brandMark">M</span>
					<span>kurbis</span>
				</div>
				<p className="eyebrow">FREE MINECRAFT HOSTING / V1</p>
				<h1>A quiet place for a loud little world.</h1>
				<p>
					One Pumpkin server per account. It sleeps when nobody is around, wakes when someone joins, and keeps the world backed up
					in between.
				</p>
				<a className="primaryButton loginButton" href={apiUrl("/auth/login")}>
					Continue with OAuth
					<span aria-hidden="true">↗</span>
				</a>
			</section>
			<aside className="loginSpec" aria-label="Free server limits">
				<div className="specBlock">
					<span>MEMORY</span>
					<strong>2 GiB</strong>
				</div>
				<div className="specBlock">
					<span>WORLD STORAGE</span>
					<strong>5 GiB</strong>
				</div>
				<div className="specBlock">
					<span>PLAYER LIMIT</span>
					<strong>None set</strong>
				</div>
				<div className="specBlock">
					<span>IDLE SLEEP</span>
					<strong>5 min</strong>
				</div>
				<div className="gridGlyph" aria-hidden="true">
					{Array.from({ length: 36 }, (_, index) => (
						<i key={index} />
					))}
				</div>
			</aside>
		</main>
	);
}

function CreateServer({ onCreated, onError }: { onCreated: () => Promise<void>; onError: (message: string) => void }) {
	const [subdomain, setSubdomain] = useState("");
	const [creating, setCreating] = useState(false);

	async function submit(event: FormEvent) {
		event.preventDefault();
		setCreating(true);
		try {
			await api("/api/server", {
				method: "POST",
				body: JSON.stringify({ subdomain }),
			});
			await onCreated();
		} catch (reason) {
			onError(reason instanceof Error ? reason.message : "Could not create server");
		} finally {
			setCreating(false);
		}
	}

	return (
		<section className="createGrid">
			<form className="createCard" onSubmit={submit}>
				<div>
					<p className="sectionLabel">01 / ADDRESS</p>
					<h2>Choose your world’s front door.</h2>
					<p>Three to thirty-two lowercase letters, digits, or dashes. You can change it later while the server is sleeping.</p>
				</div>
				<label className="domainField">
					<span>SUBDOMAIN</span>
					<input
						value={subdomain}
						onChange={(event) => setSubdomain(event.target.value.toLowerCase().replace(/[^a-z0-9-]/g, ""))}
						minLength={3}
						maxLength={32}
						pattern="[a-z0-9](?:[a-z0-9-]{1,30})[a-z0-9]"
						placeholder="my-world"
						required
						autoFocus
					/>
				</label>
				<button className="primaryButton" disabled={creating}>
					{creating ? "Creating…" : "Create server"}
					<span aria-hidden="true">→</span>
				</button>
			</form>
			<aside className="principlesCard">
				<p className="sectionLabel">WHAT HAPPENS NEXT</p>
				<ol>
					<li>
						<b>01</b>
						<span>Your server starts asleep and uses no worker capacity.</span>
					</li>
					<li>
						<b>02</b>
						<span>Press start or connect to the address to wake it.</span>
					</li>
					<li>
						<b>03</b>
						<span>When everyone leaves, the five-minute sleep timer begins.</span>
					</li>
					<li>
						<b>04</b>
						<span>A clean snapshot is stored before the worker is released.</span>
					</li>
				</ol>
			</aside>
		</section>
	);
}

function Dashboard({
	server,
	logs,
	busy,
	onAction,
	onChanged,
	onError,
}: {
	server: Server;
	logs: LogLine[];
	busy: string | null;
	onAction: (name: "start" | "stop" | "restart") => Promise<void>;
	onChanged: () => Promise<void>;
	onError: (message: string) => void;
}) {
	const usedPercent = Math.min(100, (server.storage_bytes / server.storage_limit_bytes) * 100);
	const sleeping = server.observed_state === "sleeping";
	const canStart = ["sleeping", "failed"].includes(server.observed_state);
	const canStop = !["sleeping", "stopping", "snapshotting", "deleted"].includes(server.observed_state);
	const canRestart = server.observed_state === "online";

	return (
		<div className="dashboard">
			<section className="statusCard">
				<div className="statusHeading">
					<div>
						<p className="sectionLabel">LIVE STATE</p>
						<div className="stateLine">
							<span className={`stateDot state-${server.observed_state}`} />
							<h2>{STATE_COPY[server.observed_state]}</h2>
						</div>
					</div>
					<span className="serverId">#{server.id.slice(0, 8)}</span>
				</div>
				<p className="stateDetail">{stateDetail(server)}</p>
				<div className="actions">
					<button className="primaryButton" disabled={!canStart || busy !== null} onClick={() => void onAction("start")}>
						{busy === "start" ? "Starting…" : "Start"}
						<span>▶</span>
					</button>
					<button className="secondaryButton" disabled={!canRestart || busy !== null} onClick={() => void onAction("restart")}>
						Restart
					</button>
					<button className="secondaryButton danger" disabled={!canStop || busy !== null} onClick={() => void onAction("stop")}>
						Stop
					</button>
				</div>
			</section>

			<section className="addressesCard">
				<p className="sectionLabel">CONNECT</p>
				<AddressRow label="JAVA" address={server.java_address} />
				<AddressRow label="BEDROCK" address={server.bedrock_address} />
				<p className="finePrint">Sleeping servers are woken by the gateway before a playable backend is published.</p>
			</section>

			<section className="resourceCard">
				<p className="sectionLabel">RESOURCES</p>
				<div className="resourceNumber">
					<strong>{formatBytes(server.storage_bytes)}</strong>
					<span>/ {formatBytes(server.storage_limit_bytes)}</span>
				</div>
				<div className="meter" aria-label="Storage usage">
					<span style={{ width: `${usedPercent}%` }} />
				</div>
				<div className="resourceMeta">
					<span>{usedPercent.toFixed(1)}% storage used</span>
					<span>{formatBytes(server.memory_limit_bytes)} RAM</span>
				</div>
			</section>

			<SettingsCard server={server} disabled={!sleeping} onChanged={onChanged} onError={onError} />

			<DangerZone disabled={!sleeping} onDeleted={onChanged} onError={onError} />

			<section className="consoleCard">
				<div className="consoleHeader">
					<div>
						<p className="sectionLabel">RECENT LOGS</p>
						<h3>Server console</h3>
					</div>
					<span>{logs.length} lines</span>
				</div>
				<div className="console" role="log" aria-live="off">
					{logs.length ? (
						logs.map((entry) => (
							<div className="logLine" key={entry.id}>
								<time>{new Date(entry.created_at).toLocaleTimeString()}</time>
								<span>{entry.line}</span>
							</div>
						))
					) : (
						<p className="emptyConsole">No logs yet. Start the server and this window will fill in.</p>
					)}
				</div>
			</section>
		</div>
	);
}

function DangerZone({
	disabled,
	onDeleted,
	onError,
}: {
	disabled: boolean;
	onDeleted: () => Promise<void>;
	onError: (message: string) => void;
}) {
	const [deleting, setDeleting] = useState(false);

	return (
		<section className="dangerCard">
			<div>
				<p className="sectionLabel">DANGER ZONE</p>
				<h3>Delete this server</h3>
				<p>The server must be sleeping first. This removes the hosted server record and frees the subdomain.</p>
			</div>
			<button
				className="secondaryButton danger"
				disabled={disabled === false || deleting}
				onClick={async () => {
					if (!window.confirm("Delete this server? This action cannot be undone from the panel.")) {
						return;
					}
					setDeleting(true);
					try {
						await api("/api/server", { method: "DELETE" });
						await onDeleted();
					} catch (reason) {
						onError(reason instanceof Error ? reason.message : "Could not delete server");
					} finally {
						setDeleting(false);
					}
				}}
			>
				{deleting ? "Deleting…" : "Delete server"}
			</button>
		</section>
	);
}

function AddressRow({ label, address }: { label: string; address: string }) {
	const [copied, setCopied] = useState(false);
	return (
		<div className="addressRow">
			<span>{label}</span>
			<code>{address}</code>
			<button
				onClick={async () => {
					await navigator.clipboard.writeText(address);
					setCopied(true);
					window.setTimeout(() => setCopied(false), 1400);
				}}
			>
				{copied ? "Copied" : "Copy"}
			</button>
		</div>
	);
}

function SettingsCard({
	server,
	disabled,
	onChanged,
	onError,
}: {
	server: Server;
	disabled: boolean;
	onChanged: () => Promise<void>;
	onError: (message: string) => void;
}) {
	const normalized = useMemo(() => normalizeSettings(server.settings), [server.settings]);
	const [settings, setSettings] = useState(normalized);
	const [subdomain, setSubdomain] = useState(server.subdomain);
	const [saving, setSaving] = useState(false);

	useEffect(() => {
		setSettings(normalized);
		setSubdomain(server.subdomain);
	}, [normalized, server.subdomain]);

	async function save(event: FormEvent) {
		event.preventDefault();
		setSaving(true);
		try {
			await api("/api/server", {
				method: "PATCH",
				body: JSON.stringify({ subdomain, settings }),
			});
			await onChanged();
		} catch (reason) {
			onError(reason instanceof Error ? reason.message : "Could not save settings");
		} finally {
			setSaving(false);
		}
	}

	return (
		<form className="settingsCard" onSubmit={save}>
			<div className="settingsHeader">
				<div>
					<p className="sectionLabel">CONFIGURATION</p>
					<h3>World settings</h3>
				</div>
				{!disabled && <span className="lockedPill">Sleep to edit</span>}
			</div>
			<div className="settingsGrid">
				<label>
					<span>Address</span>
					<input
						value={subdomain}
						disabled={!disabled}
						minLength={3}
						maxLength={32}
						onChange={(event) => setSubdomain(event.target.value.toLowerCase().replace(/[^a-z0-9-]/g, ""))}
					/>
				</label>
				<label className="wideSetting">
					<span>MOTD</span>
					<input
						value={settings.motd}
						maxLength={120}
						disabled={!disabled}
						onChange={(event) => setSettings({ ...settings, motd: event.target.value })}
					/>
				</label>
				<label>
					<span>Difficulty</span>
					<select
						value={settings.difficulty}
						disabled={!disabled}
						onChange={(event) =>
							setSettings({
								...settings,
								difficulty: event.target.value as NormalizedSettings["difficulty"],
							})
						}
					>
						{["Peaceful", "Easy", "Normal", "Hard"].map((value) => (
							<option key={value}>{value}</option>
						))}
					</select>
				</label>
				<label>
					<span>Gamemode</span>
					<select
						value={settings.gamemode}
						disabled={!disabled}
						onChange={(event) =>
							setSettings({
								...settings,
								gamemode: event.target.value as NormalizedSettings["gamemode"],
							})
						}
					>
						{["Survival", "Creative", "Adventure", "Spectator"].map((value) => (
							<option key={value}>{value}</option>
						))}
					</select>
				</label>
				<label>
					<span>View distance</span>
					<input
						type="number"
						min={2}
						max={32}
						value={settings.view_distance}
						disabled={!disabled}
						onChange={(event) =>
							setSettings({
								...settings,
								view_distance: Number(event.target.value),
							})
						}
					/>
				</label>
				<label>
					<span>Simulation distance</span>
					<input
						type="number"
						min={2}
						max={32}
						value={settings.simulation_distance}
						disabled={!disabled}
						onChange={(event) =>
							setSettings({
								...settings,
								simulation_distance: Number(event.target.value),
							})
						}
					/>
				</label>
				<Toggle
					label="Hardcore"
					checked={settings.hardcore}
					disabled={!disabled}
					onChange={(hardcore) => setSettings({ ...settings, hardcore })}
				/>
				<Toggle
					label="Whitelist"
					checked={settings.whitelist}
					disabled={!disabled}
					onChange={(whitelist) => setSettings({ ...settings, whitelist })}
				/>
			</div>
			<div className="settingsFooter">
				<p>Configuration is applied on the next start.</p>
				<button className="secondaryButton" disabled={!disabled || saving}>
					{saving ? "Saving…" : "Save configuration"}
				</button>
			</div>
		</form>
	);
}

function Toggle({
	label,
	checked,
	disabled,
	onChange,
}: {
	label: string;
	checked: boolean;
	disabled: boolean;
	onChange: (checked: boolean) => void;
}) {
	return (
		<label className="toggleRow">
			<span>{label}</span>
			<input type="checkbox" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
			<i aria-hidden="true" />
		</label>
	);
}

function stateDetail(server: Server) {
	switch (server.observed_state) {
		case "online":
			return "The backend is accepting players. Idle sleep starts after the last player leaves.";
		case "queued":
			return "All workers are full. Your start request is preserved and will run when capacity opens.";
		case "restoring":
			return "The latest complete world snapshot is being restored onto the selected worker.";
		case "starting":
			return "Pumpkin is booting and the gateway is waiting for explicit readiness.";
		case "stopping":
			return "New routing is closed while Pumpkin saves and exits cleanly.";
		case "snapshotting":
			return "The final world snapshot is being checksummed and published.";
		case "failed":
			return server.last_error ?? "The last lifecycle operation failed. You can retry the start.";
		case "sleeping":
			return "No worker is reserved. Your latest complete world snapshot is ready to restore.";
		default:
			return "This server is not available.";
	}
}
