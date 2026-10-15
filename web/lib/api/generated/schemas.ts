export type paths = Record<string, never>;
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        Account: {
            identities: components["schemas"]["AccountIdentity"][];
            sessions: components["schemas"]["AccountSession"][];
            worlds: components["schemas"]["AccountWorld"][];
        };
        AccountIdentity: {
            /** Format: date-time */
            created_at: string;
            display_name: string;
            email?: string | null;
            email_verified: boolean;
            /** Format: uuid */
            id: string;
            provider: string;
        };
        AccountSession: {
            /** Format: date-time */
            created_at: string;
            current: boolean;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            last_seen_at: string;
        };
        AccountSessionPath: {
            /** Format: uuid */
            session_id: string;
        };
        /** @enum {string} */
        AccountStatus: "active" | "suspended";
        AccountWorld: {
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            id: string;
            is_default: boolean;
            observed_state: components["schemas"]["ObservedState"];
            subdomain: string;
        };
        ActivityEntry: {
            type: components["schemas"]["ServerEventKind"];
            /** Format: date-time */
            created_at: string;
            detail?: string | null;
            /** Format: int64 */
            id: number;
            status?: string | null;
            summary: string;
        };
        AiActiveJob: {
            /** Format: uuid */
            job_id: string;
            /** Format: uuid */
            operator_id: string;
            state: components["schemas"]["AiJobState"];
        };
        /** @description Spending against one monthly cap. */
        AiBudget: {
            /** Format: int64 */
            budget_micros: number;
            /**
             * Format: int64
             * @description Worst-case cost of model calls that are still in flight.
             */
            reserved_micros: number;
            /** Format: int64 */
            spent_micros: number;
        };
        /** @description Gateway-wide defaults that apply when a server keeps a setting unset. */
        AiDefaults: {
            /** Format: int64 */
            account_monthly_budget_micros: number;
            effort: components["schemas"]["AiEffort"];
            model: string;
            /** Format: int64 */
            server_monthly_budget_micros: number;
        };
        /**
         * @description How much the model thinks before answering.
         * @enum {string}
         */
        AiEffort: "low" | "medium" | "high";
        AiJob: {
            answer?: string | null;
            /** Format: uuid */
            conversation_id: string;
            /** Format: int64 */
            cost_micros: number;
            /** Format: date-time */
            created_at: string;
            error?: string | null;
            /** Format: date-time */
            finished_at?: string | null;
            /** Format: uuid */
            id: string;
            model: string;
            /** Format: int32 */
            model_calls: number;
            /** Format: uuid */
            operator_id: string;
            operator_name: string;
            prompt: string;
            state: components["schemas"]["AiJobState"];
            /** Format: int32 */
            tool_calls: number;
            /** Format: date-time */
            updated_at: string;
        };
        /** @description One request with every tool call it made. */
        AiJobDetail: {
            job: components["schemas"]["AiJob"];
            tool_calls: components["schemas"]["AiToolCall"][];
        };
        AiJobEvent: {
            /** Format: int64 */
            id: number;
            /** Format: uuid */
            job_id: string;
            kind: components["schemas"]["AiJobEventKind"];
            message: string;
            /** Format: uuid */
            operator_id: string;
        };
        /**
         * @description Chat line delivered to the requesting operator.
         * @enum {string}
         */
        AiJobEventKind: "progress" | "answer" | "error";
        /** @description Start a request on behalf of an in-game operator. */
        AiJobRequest: {
            /**
             * Format: uuid
             * @description Client-chosen id; retrying with the same id returns the same job.
             */
            job_id: string;
            /**
             * @description Start a fresh conversation instead of continuing the operator's last one.
             * @default false
             */
            new_conversation: boolean;
            /** Format: uuid */
            operator_id: string;
            operator_name: string;
            prompt: string;
        };
        /**
         * @description Lifecycle of one operator request.
         * @enum {string}
         */
        AiJobState: "queued" | "running" | "awaiting_tools" | "succeeded" | "failed" | "cancelled";
        /** @description A model the server owner can choose, with its standard prices. */
        AiModel: {
            id: string;
            /**
             * Format: int64
             * @description Micro-USD per million uncached input tokens.
             */
            input_micros_per_mtok: number;
            name: string;
            /**
             * Format: int64
             * @description Micro-USD per million output tokens.
             */
            output_micros_per_mtok: number;
        };
        /** @description Everything the panel shows about the AI assistant of one server. */
        AiOverview: {
            /** @description Whether the control plane has a model provider configured. */
            available: boolean;
            defaults?: components["schemas"]["AiDefaults"] | null;
            jobs: components["schemas"]["AiJob"][];
            models: components["schemas"]["AiModel"][];
            settings: components["schemas"]["AiSettings"];
            usage: components["schemas"]["AiUsage"];
        };
        /** @description Per-server AI settings. `None` means "use the gateway default". */
        AiSettings: {
            effort?: components["schemas"]["AiEffort"] | null;
            enabled: boolean;
            model?: string | null;
            /**
             * Format: int64
             * @description Monthly spending cap for this server in micro-USD, at most the
             *      account's monthly budget.
             */
            monthly_budget_micros?: number | null;
        };
        /** @description One plugin poll: return finished tool results, receive new calls and chat
         *      lines. */
        AiSyncRequest: {
            /**
             * Format: int64
             * @description Last event id the plugin has seen. Omit it on the first sync: the
             *      plugin then receives only the lines of requests still running.
             */
            after_event_id?: number | null;
            /** @default [] */
            results: components["schemas"]["AiToolResult"][];
        };
        AiSyncResponse: {
            active_jobs: components["schemas"]["AiActiveJob"][];
            calls: components["schemas"]["AiToolCallRequest"][];
            /**
             * Format: int64
             * @description Pass back as `after_event_id` on the next sync.
             */
            cursor: number;
            events: components["schemas"]["AiJobEvent"][];
        };
        /** @description One tool call as recorded for the owner's audit trail. */
        AiToolCall: {
            call_id: string;
            /** Format: date-time */
            completed_at?: string | null;
            /** Format: date-time */
            created_at: string;
            input: unknown;
            is_error: boolean;
            name: string;
            output?: string | null;
            state: components["schemas"]["AiToolCallState"];
        };
        /** @description A tool call the plugin must execute as the requesting operator. */
        AiToolCallRequest: {
            call_id: string;
            input: unknown;
            /** Format: uuid */
            job_id: string;
            name: string;
            /** Format: uuid */
            operator_id: string;
        };
        /**
         * @description Execution state of one tool call requested by the model.
         * @enum {string}
         */
        AiToolCallState: "pending" | "delivered" | "completed";
        AiToolResult: {
            call_id: string;
            /** @default false */
            is_error: boolean;
            /** Format: uuid */
            job_id: string;
            output: string;
        };
        /** @description This calendar month's (UTC) usage of one server and its owner's account. */
        AiUsage: {
            account: components["schemas"]["AiBudget"];
            /** Format: int64 */
            cache_creation_input_tokens: number;
            /** Format: int64 */
            cache_read_input_tokens: number;
            /** Format: int64 */
            input_tokens: number;
            /** Format: int64 */
            model_calls: number;
            /** Format: int64 */
            output_tokens: number;
            /** Format: date-time */
            period_start: string;
            server: components["schemas"]["AiBudget"];
        };
        ApiErrorResponse: {
            code: string;
            message: string;
        };
        CommandResponse: {
            observed_state: components["schemas"]["ObservedState"];
        };
        /** @description A single server-console command submitted by an authorized browser actor. */
        ConsoleCommandRequest: {
            command: string;
            /** Format: uuid */
            request_id: string;
        };
        ConsoleCommandResponse: {
            /** Format: int32 */
            attempt: number;
            /** Format: date-time */
            claimed_at?: string | null;
            command: string;
            /** Format: date-time */
            completed_at?: string | null;
            /** Format: date-time */
            created_at: string;
            error?: string | null;
            /** Format: int64 */
            generation: number;
            /** Format: uuid */
            id: string;
            output: string[];
            /** Format: uuid */
            request_id: string;
            /** Format: uuid */
            server_id: string;
            status: components["schemas"]["ConsoleCommandStatus"];
            /** Format: date-time */
            updated_at: string;
        };
        /** @enum {string} */
        ConsoleCommandStatus: "queued" | "running" | "succeeded" | "failed" | "cancelled";
        /** @enum {string} */
        DesiredState: "running" | "stopped" | "deleted";
        /** @enum {string} */
        Difficulty: "Peaceful" | "Easy" | "Normal" | "Hard";
        EventsQuery: {
            /** Format: int64 */
            after_id?: number | null;
        };
        /** @enum {string} */
        GameMode: "Survival" | "Creative" | "Adventure" | "Spectator";
        InGamePlayer: {
            name: string;
            /** Format: uuid */
            player_id: string;
        };
        InGameWhitelistMutation: {
            /** @description The player in game who adds them. An operator always may; a player on
             *      the whitelist may while the server's `whitelist_by_members` setting is on. */
            added_by: components["schemas"]["InGamePlayer"];
            /** @description The player to add, as Pumpkin named them. */
            name: string;
            /**
             * Format: uuid
             * @description The UUID Pumpkin reported when it turned this player away. A Bedrock
             *      player who has never been on the server cannot be resolved by name, so
             *      the runtime that saw them vouches for it, as it does for the player
             *      roster. Without it the name is resolved like a panel addition.
             * @default null
             */
            player_id: string | null;
        };
        /** @description A player the whitelist turned away in the last day. */
        JoinRequest: {
            /**
             * Format: int32
             * @description How often they tried, as the server reported it (at most once a minute).
             */
            attempts: number;
            /** Format: date-time */
            first_attempt_at: string;
            /** Format: date-time */
            last_attempt_at: string;
            name: string;
            /** Format: uuid */
            player_id: string;
        };
        JoinRequestPath: {
            /** Format: uuid */
            player_id: string;
            /** Format: uuid */
            server_id: string;
        };
        JoinRequestReport: {
            name: string;
            /**
             * Format: uuid
             * @description The UUID Pumpkin gave the player it turned away.
             */
            player_id: string;
        };
        LimitQuery: {
            /** Format: int64 */
            limit?: number | null;
        };
        LogEntry: {
            /** Format: date-time */
            created_at: string;
            /** Format: int64 */
            id: number;
            line: string;
        };
        LogPage: {
            items: components["schemas"]["LogEntry"][];
            /** Format: int64 */
            next_after_id?: number | null;
        };
        LogQuery: {
            /** Format: int64 */
            after_id?: number | null;
            /** Format: int64 */
            limit?: number | null;
        };
        LoginLinkResponse: {
            /** Format: int64 */
            expires_in_seconds: number;
            url: string;
        };
        MarketInstallRequest: {
            /**
             * Format: int64
             * @description The market listing to install, or to update to its latest version.
             */
            market_plugin_id: number;
        };
        /** @description One free plugin listed on the Pumpkin market. */
        MarketPlugin: {
            description: string;
            category: string;
            developer: string;
            /** Format: int64 */
            downloads: number;
            /** @description HTTPS image on the market's asset host. */
            icon_url?: string | null;
            /** Format: int64 */
            id: number;
            /**
             * Format: uuid
             * @description The installed copy of this listing on the server, if any.
             */
            installed_plugin_id?: string | null;
            installed_version?: string | null;
            name: string;
            version: string;
        };
        MarketPluginQuery: {
            /** @description Matches names, developers, categories and descriptions; at most 64
             *      characters are used. */
            query?: string | null;
        };
        MarketPlugins: {
            plugins: components["schemas"]["MarketPlugin"][];
        };
        MinecraftRedeemRequest: {
            token: string;
        };
        OAuthCallback: {
            code?: string | null;
            error?: string | null;
            state: string;
        };
        /** @enum {string} */
        OAuthProvider: "google" | "discord";
        OAuthProviderPath: {
            provider: components["schemas"]["OAuthProvider"];
        };
        /** @enum {string} */
        ObservedState: "sleeping" | "queued" | "restoring" | "starting" | "online" | "stopping" | "snapshotting" | "resetting" | "failed" | "deleted";
        Operation: {
            /** Format: int32 */
            attempt: number;
            /** Format: date-time */
            created_at: string;
            error?: components["schemas"]["Problem"] | null;
            /** Format: int64 */
            generation: number;
            /** Format: uuid */
            id: string;
            kind: components["schemas"]["OperationKind"];
            message?: string | null;
            stage?: string | null;
            status: components["schemas"]["OperationStatus"];
            /** Format: date-time */
            updated_at: string;
        };
        /** @enum {string} */
        OperationKind: "start" | "stop" | "restart" | "snapshot" | "restore" | "reset" | "delete";
        /** @enum {string} */
        OperationStatus: "pending" | "running" | "succeeded" | "failed" | "cancelled";
        OperatorDeleteQuery: {
            /** Format: uuid */
            player_id: string;
        };
        OperatorEntry: {
            /** Format: date-time */
            created_at: string;
            is_owner: boolean;
            name: string;
            /** Format: uuid */
            player_id: string;
            /** Format: date-time */
            updated_at: string;
        };
        OperatorMutation: {
            /** @description Java names are resolved with Mojang. Bedrock names resolve only while
             *      the player is online on this server or already has a stored roster entry. */
            name: string;
        };
        Operators: {
            /** Format: int64 */
            applied_revision: number;
            entries: components["schemas"]["OperatorEntry"][];
            /** Format: int64 */
            revision: number;
            synchronized: boolean;
        };
        /** @enum {string} */
        PlayerEdition: "java" | "bedrock";
        PlayerPresence: {
            /** Format: date-time */
            connected_at: string;
            edition: components["schemas"]["PlayerEdition"];
            /** Format: date-time */
            last_seen_at: string;
            name: string;
            /** Format: uuid */
            player_id: string;
        };
        /**
         * @description Where an installed plugin came from.
         * @enum {string}
         */
        PluginSource: "market" | "upload";
        /**
         * @description Whether the runtime runs the installed binary. `pending` means the
         *      runtime has not loaded this binary yet; a sleeping server loads it when
         *      it next starts.
         * @enum {string}
         */
        PluginState: "pending" | "loaded" | "failed" | "disabled";
        PluginUpdateRequest: {
            enabled: boolean;
        };
        PluginUploadQuery: {
            /** @description The uploaded file's name; its stem names the plugin. */
            file_name: string;
        };
        Problem: {
            code: string;
            detail?: string | null;
            message: string;
            retryable: boolean;
        };
        /** @enum {string} */
        ResetMode: "world" | "full";
        RuntimeStats: {
            /** Format: uint64 */
            cpu_limit_millis?: number | null;
            /** Format: double */
            cpu_percent?: number | null;
            /** Format: uint64 */
            disk_limit_bytes?: number | null;
            /** Format: uint64 */
            disk_used_bytes?: number | null;
            /**
             * Format: int64
             * @description The placement generation that produced this sample.
             */
            generation: number;
            /**
             * Format: date-time
             * @description Assigned by the control plane when the sample is persisted.
             */
            measured_at: string;
            /** Format: uint64 */
            memory_limit_bytes?: number | null;
            /** Format: uint64 */
            memory_used_bytes?: number | null;
            /** Format: double */
            tps?: number | null;
        };
        Server: {
            bedrock_address: string;
            /** Format: date-time */
            created_at: string;
            current_operation?: components["schemas"]["Operation"] | null;
            /** Format: int64 */
            current_snapshot_generation?: number | null;
            desired_state: components["schemas"]["DesiredState"];
            error?: components["schemas"]["Problem"] | null;
            /** Format: int64 */
            generation: number;
            /** Format: uuid */
            id: string;
            is_default: boolean;
            java_address: string;
            latest_snapshot?: components["schemas"]["Snapshot"] | null;
            /** Format: int64 */
            memory_limit_bytes: number;
            observed_state: components["schemas"]["ObservedState"];
            /** Format: int64 */
            player_count: number;
            settings: components["schemas"]["ServerSettings"];
            stage?: string | null;
            /** Format: int64 */
            storage_bytes: number;
            /** Format: int64 */
            storage_limit_bytes: number;
            subdomain: string;
            /** Format: date-time */
            updated_at: string;
        };
        /** @description Whether a server name can still be claimed. Names are claimed only by
         *      joining their address in Minecraft, so this reserves nothing. */
        ServerAddress: {
            address: string;
            available: boolean;
            subdomain: string;
        };
        ServerAiJobPath: {
            /** Format: uuid */
            job_id: string;
            /** Format: uuid */
            server_id: string;
        };
        /** @description Canonical lifecycle command wire format at
         *      `POST /api/v1/servers/{server_id}/commands`. */
        ServerCommandRequest: {
            /** @constant */
            command: "start";
            reason?: string | null;
        } | {
            /** @constant */
            command: "stop";
            /** Format: int64 */
            generation?: number | null;
            reason?: string | null;
        } | {
            /** @constant */
            command: "restart";
        } | {
            /** @constant */
            command: "snapshot";
            /** Format: int64 */
            generation?: number | null;
            reason?: components["schemas"]["SnapshotReason"] | null;
        } | {
            /** @constant */
            command: "reset";
            mode: components["schemas"]["ResetMode"];
        } | {
            /** @constant */
            command: "delete";
        } | {
            /** @constant */
            command: "restore";
            /** Format: int64 */
            generation: number;
        };
        ServerConsoleCommandPath: {
            /** Format: uuid */
            command_id: string;
            /** Format: uuid */
            server_id: string;
        };
        ServerEvent: {
            /** Format: int64 */
            id: number;
        } & ({
            /** @constant */
            type: "server";
            data: components["schemas"]["Server"];
        } | {
            /** @constant */
            type: "operation";
            data?: components["schemas"]["Operation"] | null;
        } | {
            /** @constant */
            type: "snapshot";
            data?: components["schemas"]["Snapshot"] | null;
        } | {
            /** @constant */
            type: "log";
            data?: components["schemas"]["LogEntry"] | null;
        } | {
            /** @constant */
            type: "players";
            data: components["schemas"]["PlayerPresence"][];
        } | {
            /** @constant */
            type: "whitelist";
            data: components["schemas"]["Whitelist"];
        } | {
            /** @constant */
            type: "operators";
            data: components["schemas"]["Operators"];
        } | {
            /** @constant */
            type: "ai";
            data: components["schemas"]["AiOverview"];
        } | {
            /** @constant */
            type: "plugins";
            data: components["schemas"]["ServerPlugins"];
        });
        /** @enum {string} */
        ServerEventKind: "server" | "operation" | "snapshot" | "log" | "players" | "whitelist" | "operators" | "ai" | "plugins";
        ServerExportPath: {
            /** Format: uuid */
            export_id: string;
            /** Format: uuid */
            server_id: string;
        };
        /** @description A server id extracted from the request path together with the canonical
         *      server row.  Authorization remains actor-specific and is performed by the
         *      handler/command service; this extractor only normalizes path lookup and the
         *      not-found response. */
        ServerIdPath: {
            /** Format: uuid */
            server_id: string;
        };
        /** @description One plugin installed on a server. */
        ServerPlugin: {
            /** Format: date-time */
            created_at: string;
            enabled: boolean;
            /** @description Why the runtime could not load this binary. */
            error?: string | null;
            /** Format: uuid */
            id: string;
            /**
             * Format: int64
             * @description The market listing this plugin was installed from.
             */
            market_plugin_id?: number | null;
            name: string;
            /** @description The name and version the plugin reported when the runtime loaded it. */
            runtime_name?: string | null;
            runtime_version?: string | null;
            sha256: string;
            /** Format: int64 */
            size_bytes: number;
            source: components["schemas"]["PluginSource"];
            state: components["schemas"]["PluginState"];
            /** Format: date-time */
            updated_at: string;
            version?: string | null;
        };
        ServerPluginPath: {
            /** Format: uuid */
            plugin_id: string;
            /** Format: uuid */
            server_id: string;
        };
        ServerPlugins: {
            /** Format: int64 */
            applied_revision: number;
            /** Format: uint */
            max_plugin_bytes: number;
            /** Format: uint */
            max_plugins: number;
            plugins: components["schemas"]["ServerPlugin"][];
            /** Format: int64 */
            revision: number;
            /** @description Whether a running server has loaded the current list. */
            synchronized: boolean;
        };
        ServerRedeemRequest: {
            token: string;
        };
        ServerRedeemResponse: {
            /** Format: uuid */
            server_id: string;
        };
        ServerScopedSession: {
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            server_id: string;
        };
        ServerScopedSessionPath: {
            /** Format: uuid */
            session_id: string;
        };
        ServerSettings: {
            /** @default Normal */
            difficulty: components["schemas"]["Difficulty"];
            /** @default Survival */
            gamemode: components["schemas"]["GameMode"];
            /** @default false */
            hardcore: boolean;
            /** @default Hosted by Kurbis */
            motd: string;
            /**
             * Format: uint8
             * @default 8
             */
            simulation_distance: number;
            /** @default false */
            tab_stats: boolean;
            /**
             * Format: uint8
             * @default 12
             */
            view_distance: number;
            /** @default true */
            whitelist: boolean;
            /**
             * @description Players on the whitelist may let in someone the whitelist just turned
             *      away. Operators always can.
             * @default true
             */
            whitelist_by_members: boolean;
        };
        ServerSettingsPatch: {
            /** @default null */
            difficulty: components["schemas"]["Difficulty"] | null;
            /** @default null */
            gamemode: components["schemas"]["GameMode"] | null;
            /** @default null */
            hardcore: boolean | null;
            /** @default null */
            motd: string | null;
            /**
             * Format: uint8
             * @default null
             */
            simulation_distance: number | null;
            /** @default null */
            tab_stats: boolean | null;
            /**
             * Format: uint8
             * @default null
             */
            view_distance: number | null;
            /** @default null */
            whitelist: boolean | null;
            /** @default null */
            whitelist_by_members: boolean | null;
        };
        ServiceLimits: {
            /** Format: int64 */
            idle_seconds: number;
            /** Format: int64 */
            memory_bytes: number;
            /** Format: int64 */
            player_limit?: number | null;
            /** Format: int64 */
            storage_bytes: number;
        };
        Session: {
            limits: components["schemas"]["ServiceLimits"];
            /** @description Every server address is `<name>.<server_domain>`. */
            server_domain: string;
            user: components["schemas"]["SessionUser"];
        };
        SessionUser: {
            account_status: components["schemas"]["AccountStatus"];
            display_name: string;
            email?: string | null;
            /** Format: uuid */
            id: string;
        };
        SkinPath: {
            name: string;
        };
        Snapshot: {
            /** Format: date-time */
            created_at: string;
            /** Format: int64 */
            generation: number;
            reason: components["schemas"]["SnapshotReason"];
            /** Format: int64 */
            size_bytes: number;
        };
        /** @enum {string} */
        SnapshotReason: "periodic" | "shutdown" | "manual" | "restore" | "reset" | "export";
        SubdomainPath: {
            subdomain: string;
        };
        UpdateServerRequest: {
            settings?: components["schemas"]["ServerSettingsPatch"] | null;
            subdomain?: string | null;
        };
        Whitelist: {
            /** Format: int64 */
            applied_revision: number;
            enabled: boolean;
            entries: components["schemas"]["WhitelistEntry"][];
            /** @description Whether the owner's account has a linked Minecraft account, which is
             *      always on the whitelist and an operator. Without one the owner can
             *      mark a join request as their own. */
            owner_has_minecraft_account: boolean;
            /** @description Players turned away in the last day, most recent first. */
            requests: components["schemas"]["JoinRequest"][];
            /** Format: int64 */
            revision: number;
            synchronized: boolean;
            /** @description Whether players on the whitelist may let in someone it turned away. */
            whitelist_by_members: boolean;
        };
        WhitelistDeleteQuery: {
            /** Format: uuid */
            player_id: string;
        };
        WhitelistEntry: {
            /** @description Who put the player on the whitelist: the owner's account name for
             *      additions in the panel, the player's name for additions in game.
             *      Unknown for a login-link session and for entries older than this field. */
            added_by?: string | null;
            /** @description Whether the player was added from inside the game. */
            added_in_game: boolean;
            /** Format: date-time */
            created_at: string;
            name: string;
            /** Format: uuid */
            player_id: string;
            /** Format: date-time */
            updated_at: string;
        };
        WhitelistMutation: {
            /** @description The Minecraft name to resolve. Java names are looked up through Mojang;
             *      Bedrock names resolve only from this server's live players or stored roster. */
            name: string;
        };
        WorldExport: {
            archive_checksum?: string | null;
            /** Format: int64 */
            archive_size?: number | null;
            /** Format: date-time */
            created_at: string;
            /** Format: date-time */
            expires_at?: string | null;
            /** Format: uuid */
            id: string;
            last_error?: string | null;
            /** Format: uuid */
            server_id: string;
            /** Format: int64 */
            source_generation?: number | null;
            stage: string;
            status: string;
            /** Format: date-time */
            updated_at: string;
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export type operations = Record<string, never>;
