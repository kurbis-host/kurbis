export type paths = Record<string, never>;
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        Account: {
            identities: components["schemas"]["AccountIdentity"][];
            sessions: components["schemas"]["AccountSession"][];
            worlds: components["schemas"]["AccountWorld"][];
        };
        AccountIdPath: {
            /** Format: uuid */
            account_id: string;
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
        AccountListQuery: {
            /** Format: int64 */
            limit?: number | null;
            /** Format: int64 */
            offset?: number | null;
            /** @description Matches account and identity names and emails, or an account id exactly. */
            search?: string | null;
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
        AdminAccount: {
            account_status: string;
            /** Format: int64 */
            ai_cost_this_month_micros: number;
            /** Format: date-time */
            created_at: string;
            display_name: string;
            /** Format: uuid */
            id: string;
            identities: components["schemas"]["AdminIdentitySummary"][];
            /**
             * Format: date-time
             * @description The latest request of any of the account's browser sessions.
             */
            last_seen_at?: string | null;
            /** Format: int64 */
            servers: number;
        };
        AdminAccountCounts: {
            /** Format: int64 */
            signups_last_day: number;
            /** Format: int64 */
            signups_last_week: number;
            /** Format: int64 */
            suspended: number;
            /** Format: int64 */
            total: number;
        };
        AdminAccountDetail: {
            account: components["schemas"]["AdminAccount"];
            /** Format: int64 */
            ai_cost_total_micros: number;
            identities: components["schemas"]["AdminIdentity"][];
            /** @description Whether the account may read this dashboard. */
            is_admin: boolean;
            servers: components["schemas"]["AdminServer"][];
            /** @description Browser sessions, latest first. */
            sessions: components["schemas"]["AdminSession"][];
        };
        AdminAccountPage: {
            items: components["schemas"]["AdminAccount"][];
            /** Format: int64 */
            total: number;
        };
        AdminAi: {
            /** @description Null when no model provider is configured. */
            budgets?: components["schemas"]["AdminAiBudgets"] | null;
            /** @description The last 30 days, oldest first, every day present. */
            by_day: components["schemas"]["AdminAiDay"][];
            /** @description Requests of the last 30 days by state. */
            jobs_by_state: components["schemas"]["AdminStateCount"][];
            /** @description This month. */
            models: components["schemas"]["AdminAiModel"][];
            month: components["schemas"]["AdminAiMonth"];
            /** @description The latest 50 requests that failed or stalled. */
            problem_jobs: components["schemas"]["AdminAiJob"][];
            top_accounts: components["schemas"]["AdminAiSpender"][];
            /** @description The ten that spent most this month. */
            top_servers: components["schemas"]["AdminAiSpender"][];
        };
        /** @description The budgets the AI gateway enforces, each a month's spend in millionths of a US dollar. */
        AdminAiBudgets: {
            /** Format: int64 */
            account_monthly_micros: number;
            /** Format: uint */
            max_concurrent_calls: number;
            models: string[];
            /**
             * Format: int64
             * @description The default for a server; a server may set its own up to the account's.
             */
            server_monthly_micros: number;
        };
        AdminAiDay: {
            /** Format: int64 */
            cost_micros: number;
            /** Format: date */
            day: string;
            /** Format: int64 */
            model_calls: number;
        };
        AdminAiJob: {
            /** Format: int64 */
            cost_micros: number;
            /** Format: date-time */
            created_at: string;
            error?: string | null;
            /** Format: date-time */
            finished_at?: string | null;
            /** Format: uuid */
            id: string;
            /** Format: int32 */
            model_calls: number;
            operator_name: string;
            prompt: string;
            /** Format: uuid */
            server_id: string;
            /** Format: int32 */
            stalls: number;
            state: string;
            subdomain?: string | null;
            /** Format: int32 */
            tool_calls: number;
        };
        AdminAiModel: {
            /** Format: int64 */
            cache_creation_input_tokens: number;
            /** Format: int64 */
            cache_read_input_tokens: number;
            /** Format: int64 */
            cost_micros: number;
            /** Format: int64 */
            input_tokens: number;
            model: string;
            /** Format: int64 */
            model_calls: number;
            /** Format: int64 */
            output_tokens: number;
        };
        AdminAiMonth: {
            /** Format: int64 */
            cache_creation_input_tokens: number;
            /** Format: int64 */
            cache_read_input_tokens: number;
            /** Format: int64 */
            cost_micros: number;
            /** Format: int64 */
            input_tokens: number;
            /**
             * Format: int64
             * @description Requests started this month.
             */
            jobs: number;
            /** Format: int64 */
            model_calls: number;
            /** Format: int64 */
            output_tokens: number;
        };
        /** @description A server or account and what it spent this month. */
        AdminAiSpender: {
            /** Format: int64 */
            cost_micros: number;
            /** Format: uuid */
            id: string;
            /** Format: int64 */
            model_calls: number;
            /** @description The server's address or the account's name; null once it is deleted. */
            name?: string | null;
        };
        AdminBuild: {
            aliases: components["schemas"]["AdminBuildAlias"][];
            commit_sha: string;
            /** Format: date-time */
            created_at: string;
            failure_class?: string | null;
            failure_message?: string | null;
            failure_stage?: string | null;
            has_log: boolean;
            /** Format: uuid */
            id: string;
            /** @description Pull requests whose queued build this is. */
            pull_requests: number[];
            /** Format: date-time */
            requested_at?: string | null;
            retention_policy: string;
            /** Format: date-time */
            retry_at?: string | null;
            /** Format: int32 */
            retry_count: number;
            stage: string;
            /** Format: date-time */
            updated_at: string;
        };
        /** @description Where a build is published: a preview hostname and the ref it follows. */
        AdminBuildAlias: {
            hostname: string;
            kind: string;
            ref_name: string;
        };
        AdminBuildLog: {
            build_log?: string | null;
            commit_sha: string;
            failure_message?: string | null;
            /** Format: uuid */
            id: string;
            stage: string;
        };
        /** @description A builder slot in use. */
        AdminBuilderSlot: {
            builder_identity: string;
            /** Format: date-time */
            created_at: string;
            /** Format: date-time */
            heartbeat_at: string;
            /** Format: int64 */
            requested_memory_bytes: number;
            /** Format: int16 */
            slot: number;
        };
        AdminCi: {
            builder_slots: components["schemas"]["AdminBuilderSlot"][];
            /** @description The latest 50 builds to change. */
            builds: components["schemas"]["AdminBuild"][];
            /** @description Builds by stage: unfinished ones, and those finished in the last week. */
            builds_by_stage: components["schemas"]["AdminStateCount"][];
            github_rate_limits: components["schemas"]["AdminGitHubRateLimit"][];
            /** @description Previews not yet expired, latest change first. */
            previews: components["schemas"]["AdminPreview"][];
            /** @description Pull-request build requests: live ones, and those finished in the last week. */
            queue: components["schemas"]["AdminStateCount"][];
            repositories: components["schemas"]["AdminCiRepository"][];
            webhook_deliveries: components["schemas"]["AdminWebhookDeliveries"];
        };
        AdminCiRepository: {
            default_branch: string;
            enabled: boolean;
            /** Format: date-time */
            last_successful_poll?: string | null;
            /**
             * Format: date-time
             * @description A poller holds the repository until then.
             */
            poll_lease_expires_at?: string | null;
            repository: string;
        };
        /** @description The accounts made in one week, and how many of them had their server woken in each
         *      week after. */
        AdminCohort: {
            /** Format: int64 */
            accounts: number;
            /** @description Accounts with a wake in UTC calendar week 0 (the week of signup), 1, … up to 7
             *      after it; only weeks that have begun are listed. */
            returned: number[];
            /**
             * Format: date
             * @description Monday of the signup week.
             */
            week: string;
        };
        /** @description One UTC day. */
        AdminDay: {
            /** Format: int64 */
            ai_cost_micros: number;
            /** Format: date */
            day: string;
            /** Format: int32 */
            peak_players_online?: number | null;
            /**
             * Format: int32
             * @description Highest per-minute sample of the day; null for days nobody sampled.
             */
            peak_servers_online?: number | null;
            /** Format: int32 */
            servers_created: number;
            /** Format: int32 */
            servers_woken: number;
            /** Format: int32 */
            signups: number;
            /**
             * Format: int32
             * @description Successful wakes asked for that day, and the servers they woke.
             */
            wakes: number;
        };
        AdminFailure: {
            /** Format: date-time */
            at: string;
            kind: string;
            message: string;
            /** @description The failed row: an operation, export, plugin, command, job or build id, a pull
             *      request number, a webhook delivery or an object key. */
            reference: string;
            /** Format: uuid */
            server_id?: string | null;
            source: string;
            subdomain?: string | null;
        };
        /** @description Failures that said the same thing, ids and numbers aside. */
        AdminFailureGroup: {
            /** Format: int64 */
            count: number;
            /** Format: date-time */
            first_at: string;
            kind: string;
            /** Format: date-time */
            last_at: string;
            /** @description The latest failure's own words. */
            message: string;
            /**
             * Format: uuid
             * @description The latest failure's server, when it had one.
             */
            server_id?: string | null;
            /** Format: int64 */
            servers: number;
            signature: string;
            source: string;
            subdomain?: string | null;
        };
        AdminFailures: {
            /** @description The 100 groups seen last, latest first. */
            groups: components["schemas"]["AdminFailureGroup"][];
            /** @description The latest 100 failures. */
            recent: components["schemas"]["AdminFailure"][];
        };
        AdminGitHubRateLimit: {
            /** Format: int32 */
            consecutive_failures: number;
            /**
             * Format: date-time
             * @description Requests wait until then after GitHub said to slow down.
             */
            paused_until?: string | null;
            /** Format: int64 */
            remaining?: number | null;
            /** Format: date-time */
            reset_at?: string | null;
            scope: string;
        };
        AdminGrowth: {
            /** @description The last twelve signup weeks, oldest first. */
            cohorts: components["schemas"]["AdminCohort"][];
            /** @description Every day since the first account, oldest first. */
            days: components["schemas"]["AdminDay"][];
        };
        AdminIdentity: {
            /** Format: date-time */
            created_at: string;
            display_name: string;
            email?: string | null;
            email_verified: boolean;
            /** Format: uuid */
            minecraft_player_id?: string | null;
            provider: string;
            /** @description The provider's id for the account; Minecraft identities use the player's UUID. */
            subject: string;
        };
        /** @description A linked sign-in, as the list shows it. */
        AdminIdentitySummary: {
            display_name: string;
            email?: string | null;
            provider: string;
        };
        AdminOperation: {
            /** Format: int32 */
            attempt: number;
            /** Format: date-time */
            claimed_at?: string | null;
            /** Format: date-time */
            created_at: string;
            /** Format: int64 */
            generation: number;
            /** Format: uuid */
            id: string;
            kind: string;
            last_error?: string | null;
            /** Format: date-time */
            next_attempt_at: string;
            status: string;
            /** Format: date-time */
            updated_at: string;
            worker_name?: string | null;
        };
        AdminOperationCounts: {
            /** Format: int64 */
            failed_last_day: number;
            /**
             * Format: int64
             * @description Pending again after a failed attempt.
             */
            retrying: number;
            /** Format: int64 */
            running: number;
        };
        AdminOverview: {
            accounts: components["schemas"]["AdminAccountCounts"];
            /** Format: int64 */
            ai_cost_this_month_micros: number;
            fleet: components["schemas"]["FleetUsage"];
            github_rate_limits: components["schemas"]["AdminGitHubRateLimit"][];
            /** Format: date-time */
            measured_at: string;
            operations: components["schemas"]["AdminOperationCounts"];
            servers: components["schemas"]["AdminServerCounts"];
            servers_by_state: components["schemas"]["AdminStateCount"][];
            start_queue: components["schemas"]["AdminStartQueue"];
            storage: components["schemas"]["AdminStorage"];
            /** @description The tasks of the control plane that answered. */
            tasks: components["schemas"]["AdminTask"][];
            workers: components["schemas"]["AdminWorkerCounts"];
        };
        AdminPlacement: {
            active: boolean;
            /** Format: int32 */
            bedrock_port?: number | null;
            /** Format: int64 */
            generation: number;
            /** Format: int32 */
            java_port?: number | null;
            /** Format: date-time */
            updated_at: string;
            /** Format: uuid */
            worker_id: string;
            worker_name: string;
        };
        AdminPluginError: {
            enabled: boolean;
            name: string;
            /** Format: date-time */
            reported_at?: string | null;
            runtime_error: string;
            version: string;
        };
        AdminPreview: {
            commit_sha: string;
            /** Format: date-time */
            expires_at: string;
            hostname: string;
            /** Format: uuid */
            id: string;
            kind: string;
            /** Format: date-time */
            last_activity_at?: string | null;
            /** Format: int64 */
            players_online: number;
            ref_name: string;
            runtime_state?: string | null;
            state: string;
            worker_name?: string | null;
        };
        /**
         * @description How far back a chart looks.
         * @enum {string}
         */
        AdminRange: "hour" | "day" | "week" | "month";
        /** @description A server as the dashboard lists it. */
        AdminServer: {
            /** Format: date-time */
            created_at: string;
            desired_state: string;
            /** Format: uuid */
            id: string;
            last_error?: string | null;
            /**
             * Format: date-time
             * @description When the latest successful wake was asked for.
             */
            last_woken_at?: string | null;
            observed_state: string;
            /** Format: uuid */
            owner_id: string;
            owner_name: string;
            owner_status: string;
            /** Format: int64 */
            players_online: number;
            /** Format: int64 */
            storage_bytes: number;
            subdomain: string;
            /** Format: date-time */
            updated_at: string;
            /** Format: uuid */
            worker_id?: string | null;
            worker_name?: string | null;
        };
        AdminServerCounts: {
            /** Format: int64 */
            failed: number;
            /**
             * Format: int64
             * @description In a passing state for more than ten minutes.
             */
            stuck: number;
            /**
             * Format: int64
             * @description Servers not deleted.
             */
            total: number;
        };
        AdminServerDetail: {
            /** @description The latest 50, newest first. */
            operations: components["schemas"]["AdminOperation"][];
            placement?: components["schemas"]["AdminPlacement"] | null;
            plugin_errors: components["schemas"]["AdminPluginError"][];
            /** @description Over the range, oldest first; minutes the server was not running are missing. */
            samples: components["schemas"]["AdminServerSample"][];
            server: components["schemas"]["AdminServer"];
            /** @description Every snapshot still kept, newest first, complete or not. */
            snapshots: components["schemas"]["AdminSnapshot"][];
            start_request?: components["schemas"]["AdminStartRequest"] | null;
        };
        /** @description Which servers a list shows. */
        AdminServerFilter: ("all" | "online" | "sleeping" | "failed" | "queued") | "stuck" | "never_started";
        AdminServerPage: {
            items: components["schemas"]["AdminServer"][];
            /**
             * Format: int64
             * @description Servers matching the filter and search, across every page.
             */
            total: number;
        };
        /** @description A server over one bucket of minutes: its most players, and its average usage. */
        AdminServerSample: {
            /** Format: int64 */
            cpu_used_millis?: number | null;
            /** Format: int64 */
            memory_used_bytes?: number | null;
            /** Format: date-time */
            minute: string;
            /** Format: int32 */
            players_online: number;
            /** Format: double */
            tps?: number | null;
        };
        AdminServerStorage: {
            /** Format: uuid */
            server_id: string;
            /** Format: int64 */
            storage_bytes: number;
            subdomain: string;
        };
        AdminSession: {
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            last_seen_at: string;
        };
        AdminSnapshot: {
            complete: boolean;
            /** Format: date-time */
            created_at: string;
            /** Format: int64 */
            generation: number;
            reason: string;
            /** Format: int64 */
            size_bytes: number;
        };
        AdminStartQueue: {
            /**
             * Format: int64
             * @description Servers waiting for a worker with room.
             */
            depth: number;
            /** Format: date-time */
            oldest_requested_at?: string | null;
        };
        AdminStartRequest: {
            /**
             * Format: int64
             * @description Servers ahead of this one in the queue.
             */
            ahead: number;
            reason: string;
            /** Format: date-time */
            requested_at: string;
        };
        AdminStateCount: {
            /** Format: int64 */
            count: number;
            state: string;
        };
        AdminStorage: {
            /** Format: int64 */
            database_bytes: number;
            largest_servers: components["schemas"]["AdminServerStorage"][];
            largest_tables: components["schemas"]["AdminTableSize"][];
            /**
             * Format: int64
             * @description What servers' worlds take, as they last reported.
             */
            server_bytes: number;
            /** Format: int64 */
            snapshot_bytes: number;
            /**
             * Format: int64
             * @description Complete snapshots and their total size.
             */
            snapshots: number;
        };
        AdminTableSize: {
            /**
             * Format: int64
             * @description Table, indexes and TOAST.
             */
            bytes: number;
            name: string;
            /**
             * Format: int64
             * @description Rows by the planner's estimate.
             */
            rows: number;
        };
        /** @description One of the control plane's background tasks (start queue, maintenance, CI poller…). */
        AdminTask: {
            /** Format: uint32 */
            consecutive_failures: number;
            last_error?: string | null;
            /** Format: date-time */
            last_heartbeat?: string | null;
            /** Format: date-time */
            last_success?: string | null;
            name: string;
            /** @description The control plane is unhealthy while a required task is not running. */
            required: boolean;
            state: components["schemas"]["AdminTaskState"];
        };
        /** @enum {string} */
        AdminTaskState: "starting" | "running" | "backing_off" | "fatal" | "stopped";
        AdminWebhookDeliveries: {
            /** Format: int64 */
            failing: number;
            /** Format: int64 */
            pending: number;
        };
        AdminWorker: {
            address: string;
            /**
             * Format: int64
             * @description What its running servers use, from their fresh samples; null when none reported.
             */
            cpu_used_millis?: number | null;
            draining: boolean;
            /** Format: date-time */
            heartbeat_at: string;
            /** Format: uuid */
            id: string;
            /**
             * Format: date-time
             * @description When the running worker process's exclusive lease runs out.
             */
            lease_expires_at?: string | null;
            /** @description Heartbeat within the last 30 seconds: it may take new servers. */
            live: boolean;
            /** Format: int64 */
            memory_used_bytes?: number | null;
            name: string;
            /** Format: int32 */
            port_max: number;
            /** Format: int32 */
            port_min: number;
            /**
             * Format: int64
             * @description Ports leased to servers and previews.
             */
            ports_leased: number;
            previews: components["schemas"]["AdminWorkerPreview"][];
            /** Format: int64 */
            reserved_cpu_millis: number;
            /** Format: int64 */
            reserved_disk_bytes: number;
            /** Format: int64 */
            reserved_memory_bytes: number;
            runtime_architecture: string;
            servers: components["schemas"]["AdminWorkerServer"][];
            supports_ci_previews: boolean;
            /** Format: int64 */
            total_cpu_millis: number;
            /** Format: int64 */
            total_disk_bytes: number;
            /** Format: int64 */
            total_memory_bytes: number;
        };
        AdminWorkerCounts: {
            /** Format: int64 */
            draining: number;
            /**
             * Format: int64
             * @description Heartbeat within the last 30 seconds.
             */
            live: number;
            /** Format: int64 */
            total: number;
        };
        /** @description A Pumpkin preview placed on a worker. */
        AdminWorkerPreview: {
            hostname?: string | null;
            /** Format: int64 */
            players_online: number;
            /** Format: uuid */
            preview_id: string;
            state: string;
        };
        /** @description A server placed on a worker. */
        AdminWorkerServer: {
            active: boolean;
            /** Format: uuid */
            id: string;
            observed_state: string;
            /** Format: int64 */
            players_online: number;
            subdomain: string;
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
        BuildIdPath: {
            /** Format: uuid */
            build_id: string;
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
        /** @description The service in one minute, as it was sampled at the start of that minute, or in a
         *      bucket of minutes starting at `minute` (see [`history`]). */
        FleetSample: {
            /** Format: int64 */
            cpu_total_millis?: number | null;
            /** Format: int64 */
            cpu_used_millis?: number | null;
            /** Format: int64 */
            memory_total_bytes?: number | null;
            /** Format: int64 */
            memory_used_bytes?: number | null;
            /** Format: date-time */
            minute: string;
            /** Format: int64 */
            players_online: number;
            /** Format: int64 */
            servers_online: number;
            /** Format: double */
            tps?: number | null;
        };
        /** @description The whole service now, and how it got here. */
        FleetStats: {
            /** Format: int64 */
            cpu_total_millis?: number | null;
            /** Format: int64 */
            cpu_used_millis?: number | null;
            /** @description The last two hours, a sample a minute, oldest first. A minute nobody sampled is missing. */
            history: components["schemas"]["FleetSample"][];
            /** Format: date-time */
            measured_at: string;
            /** Format: int64 */
            memory_total_bytes?: number | null;
            /** Format: int64 */
            memory_used_bytes?: number | null;
            /** Format: int64 */
            players_online: number;
            /** @description Servers there were at the end of each of the last thirty days, oldest first; the last is today so far. */
            servers_by_day: number[];
            /**
             * Format: int64
             * @description Servers made and not deleted.
             */
            servers_created: number;
            /** Format: int64 */
            servers_online: number;
            /** Format: double */
            tps?: number | null;
        };
        /** @description The running servers' totals at one moment. */
        FleetUsage: {
            /**
             * Format: int64
             * @description CPU of every live worker, in thousandths of a core.
             */
            cpu_total_millis?: number | null;
            /**
             * Format: int64
             * @description CPU the running servers use, in thousandths of a core.
             */
            cpu_used_millis?: number | null;
            /** Format: int64 */
            memory_total_bytes?: number | null;
            /** Format: int64 */
            memory_used_bytes?: number | null;
            /** Format: int64 */
            players_online: number;
            /** Format: int64 */
            servers_online: number;
            /**
             * Format: double
             * @description The running servers' average ticks per second.
             */
            tps?: number | null;
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
        RangeQuery: {
            range?: components["schemas"]["AdminRange"];
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
        ServerDetailQuery: {
            range?: components["schemas"]["AdminRange"];
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
        ServerListQuery: {
            filter?: components["schemas"]["AdminServerFilter"];
            /** Format: int64 */
            limit?: number | null;
            /** Format: int64 */
            offset?: number | null;
            /** @description Matches addresses and owner names, or a server or owner id exactly. */
            search?: string | null;
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
            /** @description The account may read the operator dashboard and every server. */
            is_admin: boolean;
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
