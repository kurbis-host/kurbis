# Configuration

Runtime configuration is loaded through environment variables. See [`.env.example`](../.env.example) and [`src/config.rs`](../src/config.rs) for available settings and defaults.

The control plane requires a Google OAuth client and a Discord OAuth application. Set `GOOGLE_OAUTH_CLIENT_ID`, `GOOGLE_OAUTH_CLIENT_SECRET`, `DISCORD_OAUTH_CLIENT_ID`, and `DISCORD_OAUTH_CLIENT_SECRET` in its runtime environment. Keep client secrets in the deployment secret store; never put production credentials in source control. Register these exact callback URLs with the providers:

```text
https://<public-panel-origin>/auth/google/callback
https://<public-panel-origin>/auth/discord/callback
```

Google requests `openid email profile`; Discord requests only `identify email`. The control plane supplies each provider's standard authorization, token, and profile endpoints. The corresponding `*_OAUTH_AUTHORIZE_URL`, `*_OAUTH_TOKEN_URL`, and `*_OAUTH_USERINFO_URL` settings can point to a local OAuth fixture during development and testing.

The in-game AI assistant is optional. Set `AI_PROVIDER` to offer it; without a provider `/ai` reports that the assistant is not set up and the control plane starts no AI worker. `AI_PROVIDER=anthropic` calls Anthropic's Messages API with `AI_API_KEY`, with prompt caching, progress updates between tool calls and server-side fallbacks; `AI_BASE_URL` defaults to `https://api.anthropic.com/v1`. `AI_PROVIDER=openai` calls any OpenAI-compatible Chat Completions API, such as Ollama, vLLM, LiteLLM or Open WebUI, at `AI_BASE_URL` (for example `http://ollama:11434/v1`), sending `AI_API_KEY` as a bearer token when it is set. `AI_MODELS` lists the models owners can choose, comma-separated, and its first entry is the default; it defaults to `claude-opus-5-5,claude-sonnet-5-5,claude-haiku-5-5` for Anthropic and is required for OpenAI-compatible endpoints. Claude models are charged at Anthropic's prices and other models only when the entry prices them as `id=INPUT/OUTPUT` in US dollars per million tokens (`gpt-oss-120b=0.15/0.6`); a configured price also replaces Anthropic's. `AI_DEFAULT_EFFORT` (`low`, `medium` or `high`; default `medium`) applies to servers whose owner has not chosen otherwise. `AI_SERVER_MONTHLY_BUDGET_USD` (default 5) and `AI_ACCOUNT_MONTHLY_BUDGET_USD` (default 20) cap spending per calendar month in UTC; a server's budget never exceeds its owner's account budget. `AI_MAX_CONCURRENT_CALLS` (default 16) bounds model calls in flight across all servers. `control-plane check-ai` runs one tool round trip with every offered model using the same settings and exits non-zero when one fails, so it verifies a deployment from inside its control plane container.

The production deployment uses PostgreSQL for durable state and an S3-compatible object store for snapshots.
