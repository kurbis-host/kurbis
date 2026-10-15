/**
 * What the landing page says, once. The film shows these as type in the world and in the
 * game's interface; the page's own HTML (components/landing/mirror.tsx) says the same in
 * reading order, for screen readers, search engines and anyone the film does not reach.
 * A test holds the two together.
 */

/** Steve's line from the Minecraft movie, which he also shouts at a chicken jockey (`talk.jockey`). */
const JOCKEY = "Chicken jockey!";
/** His line from it that he starts with in the cave, where he mines (`talk.cave.mines`). */
const MINES = "As a child, I yearned for the mines.";
/**
 * Who comes to the homestead's gate (film/homestead.ts): the oldest of the community's
 * Minecraft creepypastas, who looks like Steve but for his eyes, blank and shining, and
 * whom the patch notes have said for years was removed. (He is at the treeline too, by the
 * hero, for a visitor who stays: film/sighting.ts.)
 */
const HEROBRINE = "Herobrine";

export const COPY = {
	/** The bar along the top of the film, and the same links at the top of the page's HTML. */
	bar: {
		signIn: "Sign in",
		get: "Get your server",
		/** For a visitor who is back: their server is made. */
		home: "Your server",
	},
	/**
	 * What either name field in the film says of a free name: a splash, as on the game's title
	 * screen. (The page's own HTML says it plainly: free, and Enter claims it.)
	 */
	splash: "Now with 100% more pumpkin!",
	/**
	 * The splash's tooltip, when the pointer finds it (or a finger taps it): what a Kurbis
	 * server runs on, a line at a time, and a link to it (film/splash.ts). It promises no more
	 * than the book's "Is it vanilla?" does. The page's own HTML says it after the sky's name
	 * field, its lines as one paragraph, with the link.
	 */
	hood: {
		title: "Pumpkin under the hood",
		lines: [
			"Kurbis runs PumpkinMC,",
			"a Minecraft server written in Rust.",
			"Incredibly light on RAM and CPU,",
			"with almost all of vanilla's features.",
		],
		link: ["pumpkinmc.org", "https://pumpkinmc.org/"],
	},
	/**
	 * The game's own splash for Halloween, which the hero's title has on that day
	 * (film/halloween.ts), its lower case set in smaller capitals (film/splash.ts).
	 * Decoration: the page's own HTML does not repeat it.
	 */
	spooky: "OOoooOOOoooo! Spooky!",
	/**
	 * Names that do in either name field what they do in the game (film/eggs.ts), as they are
	 * typed. Decoration: the page's own HTML does not repeat them.
	 */
	eggs: {
		/** Anything called Dinnerbone or Grumm is upside down. */
		upsideDown: ["dinnerbone", "grumm"],
		/** A sheep called jeb_ runs through the wool colours. (A name cannot hold the "_".) */
		rainbow: "jeb",
		/** As every patch note has said for years: "Removed Herobrine". The field says it before what it found. */
		removed: { name: "herobrine", said: "Removed." },
		/** The name does what the creeper does to the house. */
		creeper: "creeper",
	},
	/** The card that holds a free name: to copy into the game, or to sign in with. */
	claim: {
		said: "Join it in Minecraft and it's yours the moment you arrive. No card needed.",
		copied: "Copied. In Minecraft: Multiplayer > Add Server, paste, and join.",
		/** When the name is a Java player's too, and the film shows them in their skin. */
		you: "That's you, right?",
		/** While the name in the card is being checked, or when it turns out it cannot be had. */
		checking: "Checking...",
		taken: "Taken. Try another name.",
		short: "Use at least 3 characters.",
		dash: "No dash at the start or end.",
		/** A visitor back with the name they claimed, which is theirs now. */
		home: "Welcome back. Your world is waiting.",
		panel: "Open panel",
		copy: "Copy address",
		done: "Copied!",
		signIn: "Or sign in",
		close: "Close",
	},
	sky: {
		title: ["Your world", "is waiting below"],
		caption: ["Free Minecraft servers", "for you and your friends"],
		cue: "Dig down",
		/** Under the name field, until something is typed: how, with a keyboard and without. */
		typeHint: "Type your Minecraft name",
		tapHint: "Tap to type your Minecraft name",
	},
	surface: {
		caption: "You choose the name.",
		names: ["steve", "lena", "bigfarm"],
		host: ".kurbis.host",
		editions: "Different editions. Same pond.",
		tags: ["Java", "Bedrock"],
		/**
		 * A friend asks for the address, and finds it is their own SMP (survival multiplayer:
		 * what players call a private server for friends). `{friend}` is the friend who asks,
		 * `{you}` the visitor ("You", or their name once they are a player in the film),
		 * `{address}` their server's.
		 */
		chat: [
			"<{friend}> what's the ip",
			"<{you}> {address}",
			"<{friend}> that's it?",
			"<{you}> that's it",
			"<{friend}> we have our own smp now??",
		],
	},
	dig: {
		facts: [
			["One server each.", "2 GB RAM. 5 GB world."],
			["Bigger servers are paid.", "They pay for yours."],
		],
		finds: [
			["Fossil", "Older than your last world."],
			["Chest", "Someone's coal. Finders keepers."],
			["Old sign", "It says: Back in 5 minutes."],
			["Lost sock", "The other one is in the Nether."],
		],
		/**
		 * The sign where the pumpkin starts to dig, which says the game's first rule (and it
		 * digs straight down beside it): what its board says, a line at a time, and what it
		 * says when the pointer finds it. Decoration: the page's own HTML does not repeat it.
		 */
		rule: { sign: ["Never dig", "straight", "down"], find: ["Sign", "Never dig straight down."] },
	},
	cave: {
		awake: ["Stops when you leave.", "Starts when you join."],
		/** (Phantoms come for players who do not sleep. The chat still says the world was saved.) */
		asleep: ["Nobody on. Server off.", "No phantoms."],
		/**
		 * On the action bar, when the lever is pulled while it is still doing what the last pull
		 * asked: what the game says of a bed it will not let you sleep in, as it writes it.
		 * Decoration: the page's own HTML does not repeat it.
		 */
		busy: "You may not rest now; there are monsters nearby",
		/** What the page's own lever says the state is (the film shows it). */
		now: "The server is running.",
		lever: {
			awake: ["Pull it: everyone leaves,", "and the server stops."],
			asleep: ["Pull it: someone joins,", "and the server starts."],
		},
		/** The server, in the chat, before the visitor has named theirs. */
		server: "Your server",
		/** What the chat says of the server once it is asleep, after its name (the lever, and the chat's /stop: film/chat.ts). */
		sleeps: "is asleep",
	},
	/** The cart ride from the cave to the homestead. */
	ride: {
		/**
		 * On the action bar as the pumpkin gets into the cart, and again if the visitor presses
		 * Shift on the way: what the game says on getting into a minecart, as it writes it (Java
		 * Edition, the default key). Decoration: the page's own HTML does not repeat it.
		 */
		dismount: "Press Left Shift to Dismount",
	},
	/**
	 * The homestead: three features, each played on the same yard. The headlines stand in
	 * the world (the art sets them); the rest is the interface's.
	 */
	homestead: {
		heads: [
			["Friends in.", "Everyone else out."],
			["Add plugins.", "Or write your own."],
			["Creeper? Aw man.", "Turn back time."],
		],
		/** What the page's own HTML says of each, in full. */
		said: [
			"A whitelist and operators: your friends get in, everyone else is turned away.",
			"Install free plugins from the Pumpkin market with /plugins, or upload your own.",
			"Backups of the whole world, every time it stops. Restore one and it is as it was.",
		],
		/** Herobrine at the gate: his tooltip (his name, and its lines), and what the chat says of him, as the game would. */
		whitelist: {
			stranger: HEROBRINE,
			kept: "Not on the whitelist",
			letIn: "Click to let them in",
			turned: `${HEROBRINE} was turned away: not whitelisted`,
			added: `${HEROBRINE} was added to the whitelist`,
			/** Let in, he puts the lights out, and goes: in the yellow the chat has for comings and goings. */
			left: `${HEROBRINE} left the game`,
		},
		market: {
			title: "Plugin market",
			plugins: ["Lanterns", "Fireflies", "Fireworks"],
			install: "Install",
			installed: "Installed",
			said: "installed. No restart.",
		},
		rewind: {
			/** The blast, in the chat, as the game words a death: who, how, and by whom. */
			boom: "House was blown up by Creeper",
			/**
			 * Time turned back: "Restoring backup from" a year, counting back from this one to
			 * `from`, the year of "Revenge" (the "Creeper, aw man" song) and of Minecraft 1.0.
			 */
			restoring: "Restoring backup from",
			from: 2011,
			restored: "World restored",
		},
	},
	nether: {
		headline: "Pick a name.",
		caption: "Step through to your world.",
		/**
		 * The dig's lost sock says the other one is in the Nether (`dig.finds`): it lies by the
		 * second frame, and says this when the pointer finds it. Decoration: the page's own
		 * HTML does not repeat it.
		 */
		sock: ["Lost sock", "Found it."],
	},
	/** Home again, at the end of the film: the signs, and the book of questions. */
	ending: {
		title: "See you in there.",
		/** Each sign's two lines, and the same as a sentence for the page's own HTML. */
		specs: [
			{ sign: ["One server", "per player"], said: "One server per player." },
			{ sign: ["2 GB RAM", "5 GB world"], said: "2 GB of RAM and 5 GB of world." },
			{ sign: ["Java and", "Bedrock"], said: "Java and Bedrock players join the same world." },
			{ sign: ["Starts in", "about 5 s"], said: "It starts in about 5 seconds when someone joins." },
			{ sign: ["Stops after", "5 min empty"], said: "It stops after 5 minutes with nobody on." },
			{ sign: ["No player", "limit"], said: "No player limit." },
		],
		book: "Questions",
		faq: [
			["How fast does it start?", "About 5 seconds. Join the address and you wait in a lobby while it starts."],
			[
				"Why is it free?",
				"An empty server sleeps, so it costs almost nothing to keep. That is also why it is one server each, with 2 GB of RAM.",
			],
			["Who pays for it?", "Players who need more RAM and CPU upgrade to a paid server. That pays for the free ones."],
			[
				"Is it vanilla?",
				"It runs Pumpkin, a fast Minecraft server written in Rust. Most of vanilla works; some things are still missing.",
			],
			["Can I add plugins?", "Yes. Install free ones from the Pumpkin market with /plugins, or upload your own WebAssembly plugins."],
			["How many players?", "No limit is set. The 2 GB of RAM is what limits it."],
			[
				"What happens when it stops?",
				"Your world is saved before the server stops, and comes back exactly as you left it. Older backups are kept too.",
			],
		],
		/** Its pages are numbered: "Page 2 of 6". */
		page: ["Page", "of"],
	},
	/**
	 * Advancements, as the game's toasts in the interface's corner: "Advancement made!" and
	 * the title. Decoration: the page's own HTML does not repeat them.
	 */
	toasts: {
		/** As the scroll reaches each place: the meadow, the dig, the cave, the portal. */
		scrolled: ["Touched grass", "Dirt enthusiast", "Found the cave", "We need to go deeper"],
		/** For what the visitor makes happen, by what happened (film/interface.ts, `toast`). */
		events: {
			/** A tree at the pond was punched: the game's oldest achievement. */
			wood: "Getting Wood",
			/** The cave went to sleep, whoever pulled the lever: the game's for lying in a bed. */
			dreams: "Sweet Dreams",
			/** The pumpkin rode the cart through the tunnel: the game's old achievement for going far by minecart. */
			rail: "On A Rail",
			/** Herobrine was turned away at the gate: the game's for deflecting a projectile with a shield. */
			shield: "Not Today, Thank You",
			/**
			 * The credits have rolled to their end: the game's for the End, and under it its
			 * description, which is where the page goes next (film/credits.ts).
			 */
			end: "The End",
			beginning: "Or the beginning?",
		},
	},
	/**
	 * What the friends say when clicked, in a speech bubble over their head (film/speech.ts):
	 * each place has its own lines for each of them, and the homestead a set for each of its
	 * features. A place's lines are shuffled once a visit and said in turn, round again when
	 * they run out, and go on where they left off when the visitor is back. Some are said only
	 * once something has happened there (a boot, Herobrine gone, the house back): at the next
	 * clicks after it, in order. Whoever wears the visitor's skin says the same, so no line
	 * says who says it, but for the movie's "I... am Steve.".
	 *
	 * Steve is Steve as Jack Black plays him in the Minecraft movie (2025): its lines, where
	 * they fit, and more in its manner. Alex talks as a server's chat does: lower case, no
	 * full stop. Decoration: the page's own HTML does not repeat them. (The interface's pixel
	 * face has no "…": three full stops stand in for it.)
	 */
	talk: {
		/** Shouted by Steve when a chicken jockey runs past the pond (film/jockey.ts): one of his lines there too. */
		jockey: JOCKEY,
		/** Said by whoever fishes up a boot at the pond (film/life.ts): one of their lines there too. */
		fishing: "caught a boot. again.",
		/** The pond, where both of them fish, one on Java and one on Bedrock: a new server's first moments. */
		pond: {
			Steve: ["I... am Steve.", "Welcome... to the Overworld!", "Water bucket... release!", JOCKEY, "Java and Bedrock... TOGETHER!"],
			Alex: ["who's on?", "java or bedrock?", "step one: punch tree", "what's the seed?", "afk, fishing for mending", "brb, dinner"],
			/** Once a boot has come up (a boot from fishing is never enchanted). */
			boot: { Steve: ["This... is a boot."], Alex: ["not even enchanted"] },
		},
		/** The cave, where Steve mines diamonds and Alex cannot keep still. Steve starts with `mines`. */
		cave: {
			Steve: [
				MINES,
				"First we mine, then we craft.",
				"Diamond armor. Full set!",
				"These diamonds... are MINE.",
				"What... was that noise?",
				"Torches. MORE torches!",
			],
			Alex: [
				"one more block",
				"sleep pls",
				"diamonds!! oh wait, lapis",
				"strip mining at y -59",
				"lava down here, careful",
				"my pick's about to break",
			],
			mines: MINES,
			/** Once the server has slept and woken again (the lever), and they are back. */
			woke: { Steve: ["Who... pulled the lever?"], Alex: ["that was a quick nap"] },
		},
		/** The homestead's gate, where a stranger is kept out: Herobrine. */
		gate: {
			Steve: ["No. That's a legend.", "That... is not Steve.", "Comin' in hot!", "Whitelist... ON!", "Do NOT... let him in."],
			Alex: [
				"i swear i saw herobrine",
				"can i have op",
				"who's that at the gate?",
				"is he whitelisted?",
				"why are his eyes like that",
				"who dug that 2x2 tunnel",
				"no randoms pls",
			],
			/** Once Herobrine, let in, has left the game. */
			left: { Steve: ["He... was never here."], Alex: ["did anyone else see that?", "ok who whitelisted him"] },
		},
		/** The plugin market, by the lectern: lanterns, fireflies, fireworks. */
		market: {
			Steve: [
				"This... is a crafting table.",
				"Let there be... lanterns!",
				"A book... of PLUGINS!",
				"Fireflies! At LAST!",
				"Fireworks... for ME?",
			],
			Alex: [
				"is this vanilla?",
				"no restart? nice",
				"is there /sethome?",
				"fireworks? elytra time",
				"fireflies!! don't tell the frogs",
				"lanterns > torches",
			],
		},
		/** Night, when a creeper blows up the house and time is turned back to a backup from 2011. */
		night: {
			Steve: ["Flint and steel!", "That... is not a pig.", "Night... time for BED!", "It's gonna... BLOW!", "Backup... RESTORE!"],
			Alex: [
				"it's 3am",
				"we need a cat",
				"light up the yard pls",
				"did you hear a hiss?",
				"when did we last back up?",
				"rollback pls",
			],
			/** Once the house is blown up, until it is back. */
			blown: { Steve: ["Not... the HOUSE!"], Alex: ["nooo my house"] },
			/** Once the house is back as it was in 2011 (netherite came in 2020). */
			restored: { Steve: ["The house... it LIVES!"], Alex: ["wait, is my chest still there?", "2011?? where's my netherite"] },
		},
	},
	/**
	 * The credits, which roll up a dark screen after home's last view, as the game's do after
	 * the End (film/credits.ts): how Kurbis started, and what it is built on. In the film each
	 * heading is in the game's yellow and in capitals; places on other sites (Pumpkin's, and
	 * Kurbis's own on GitHub) open in a tab of their own. The footer's links and its line close
	 * the roll. The page's own HTML says all of it too, as its footer: it is the page's own story.
	 */
	credits: {
		parts: [
			[
				"How it started",
				[
					"Kurbis began on 30 September 2026 as a test server for Pumpkin plugins. A day later it hosted whole worlds.",
					"Servers that sleep when nobody is on and wake when someone joins. This is the future of hosting.",
				],
			],
			[
				"Built on Pumpkin",
				[
					"A Minecraft server written in Rust, started by Alexander Medvedev (Snowiiii) in 2024. With Edvin Bryntesson, kralverde, lukas0008 and more than 300 contributors.",
				],
			],
		],
		community: "The community",
		links: [
			["Pumpkin's Discord", "https://discord.gg/wT8XjrjKkf"],
			["Pumpkin on GitHub", "https://github.com/Pumpkin-MC/Pumpkin"],
		],
	},
	/**
	 * The game's own keys, which players press without thinking (film/debug.ts, film/film.ts),
	 * and what the game says when they are pressed, worded as it words them. `{name}` is a
	 * value the film fills in. Decoration: the page's own HTML does not repeat them.
	 */
	keys: {
		/** F2: in the chat, before the file's name (which is underlined, as the game has it). */
		screenshot: "Saved screenshot as",
		/** F3+B: in the chat, after the prefix the game gives what its debug keys say (in its yellow, bold). */
		prefix: "[Debug]:",
		hitboxes: { shown: "Hitboxes: shown", hidden: "Hitboxes: hidden" },
		/** F3: the debug screen's lines. The first names what runs, as the game's names the game and its version. */
		screen: {
			film: "Kurbis (landing film {version})",
			fps: "{fps} fps",
			xyz: "XYZ: {x} / {y} / {z}",
			block: "Block: {x} {y} {z} [{inX} {inY} {inZ}]",
			chunk: "Chunk: {x} {y} {z} [{inX} {inZ} in r.{regionX}.{regionZ}.mca]",
			facing: "Facing: {direction} ({towards}) ({yaw} / {pitch})",
			/** (In a frame too narrow for all of that: without which way along the axes it is.) */
			facingShort: "Facing: {direction} ({yaw} / {pitch})",
			/** Each way it can face: as the game names it, and which way along its axes that is. */
			towards: {
				south: ["south", "Towards positive Z"],
				west: ["west", "Towards negative X"],
				north: ["north", "Towards negative Z"],
				east: ["east", "Towards positive X"],
			},
			biome: "Biome: {biome}",
			scroll: "Scroll: {at} / {end} screens",
			mem: "Mem: {share}% {used}/{max}MB",
			allocated: "Allocated: {share}% {size}MB",
			cpu: "CPU: {count}x",
			display: "Display: {width}x{height} ({vendor})",
			ratio: "Pixel ratio: {ratio}",
			entity: "Targeted Entity",
			server: "Server: Pumpkin",
		},
	},
	/**
	 * The game's chat, which "/" opens (film/chat.ts), and the few commands it takes: what each
	 * says, worded as the game words it. `{name}` and the like are values the film fills in.
	 * Decoration: the page's own HTML does not repeat them.
	 */
	chat: {
		/** What /help lists, a line each, as the game lists what its commands take. */
		help: ["/time set <day|night>", "/weather <clear|rain>", "/stop", "/kill", "/seed", "/help"],
		/** /time set: the game's number for the time of day it was set to. */
		time: "Set the time to {time}",
		weather: { clear: "Set the weather to clear", rain: "Set the weather to rain" },
		/** /stop, before the server goes to sleep. */
		stopping: "Stopping the server",
		/** /kill: the game's death message for it, which is also what its death screen says the death was. */
		killed: "{name} was killed",
		/** The death screen: its title, the score under it (`{score}` in yellow), and its two buttons. */
		died: "You Died!",
		score: "Score: {score}",
		respawn: "Respawn",
		title: "Title Screen",
		/** /seed (`{seed}` in green, as the game shows a seed, between the brackets). */
		seed: "Seed: [{seed}]",
		/** A command the game cannot run: what is wrong, in red; and under it, where in the line, with the game's pointer after it. */
		unknown: "Unknown or incomplete command, see below for error",
		incorrect: "Incorrect argument for command",
		here: "<--[HERE]",
		/** A line without a slash, as the game shows what a player says. */
		said: "<{name}> {text}",
		/** Who the visitor is in the chat until they are a player in the film (their name, once they are). */
		you: "You",
	},
	/**
	 * The page's last links, Kurbis's own places (its GitHub opens in a tab of its own), and its
	 * line: at the end of the credits. The line closes the legal pages too.
	 */
	footer: {
		links: [
			// (Kurbis's organisation rather than a repository: the code itself is private.)
			["Kurbis on GitHub", "https://github.com/kurbis-host"],
			["Impressum", "/impressum"],
			["Privacy", "/privacy"],
		],
		disclaimer:
			"Kurbis is not an official Minecraft or PumpkinMC product. It is not approved by or associated with Mojang or Microsoft.",
	},
} as const;
