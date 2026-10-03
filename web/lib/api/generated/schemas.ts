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
        CreateServerRequest: {
            /** @default {
             *       "difficulty": "Normal",
             *       "gamemode": "Survival",
             *       "hardcore": false,
             *       "motd": "Hosted by Kurbis",
             *       "simulation_distance": 8,
             *       "view_distance": 12,
             *       "whitelist": false
             *     } */
            settings: components["schemas"]["ServerSettings"];
            subdomain: string;
        };
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
        Problem: {
            code: string;
            detail?: string | null;
            message: string;
            retryable: boolean;
        };
        /** @enum {string} */
        ResetMode: "world" | "full";
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
        });
        /** @enum {string} */
        ServerEventKind: "server" | "operation" | "snapshot" | "log" | "players" | "whitelist" | "operators";
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
            /**
             * Format: uint8
             * @default 12
             */
            view_distance: number;
            /** @default false */
            whitelist: boolean;
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
            /**
             * Format: uint8
             * @default null
             */
            view_distance: number | null;
            /** @default null */
            whitelist: boolean | null;
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
            user: components["schemas"]["SessionUser"];
        };
        SessionUser: {
            account_status: components["schemas"]["AccountStatus"];
            display_name: string;
            email?: string | null;
            /** Format: uuid */
            id: string;
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
        UpdateServerRequest: {
            settings?: components["schemas"]["ServerSettingsPatch"] | null;
            subdomain?: string | null;
        };
        Whitelist: {
            /** Format: int64 */
            applied_revision: number;
            enabled: boolean;
            entries: components["schemas"]["WhitelistEntry"][];
            /** Format: int64 */
            revision: number;
            synchronized: boolean;
        };
        WhitelistDeleteQuery: {
            /** Format: uuid */
            player_id: string;
        };
        WhitelistEntry: {
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
