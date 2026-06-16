# miru

A local MCP server that gives LLMs direct read access to Grafana Loki logs and Prometheus metrics. Add it to any MCP host (Claude Code, Codex, Gemini CLI, and others) and ask for logs or metrics in plain language.

## How it works

An MCP host launches `miru` as a subprocess and talks to it over stdio. When you ask "show me errors from the auth service in the last 30 minutes", the model calls `miru`'s tools, which query Loki over HTTP and return the matching log lines into the conversation. Ask about a CPU spike instead and it queries Prometheus, summarising each series before handing it back.

Logs and metrics are independent: configure `[loki]`, `[prometheus]`, or both. Serving both at once requires a Grafana instance, since the two datasources are proxied through it.

## Installation

```bash
cargo install miru-mcp
```

The crate is `miru-mcp`. The command is `miru`.

Or build from git:

```bash
git clone https://github.com/ConeDjordjic/miru
cd miru
cargo build --release
cp target/release/miru ~/.local/bin/
```

## Setup

### 1. Get credentials

**Grafana instance (self-hosted or Grafana Cloud UI):**
In Grafana: **Users and Access > Service Accounts > Add service account token**
Read-only scope is sufficient. Copy the token (starts with `glsa_`).

**Grafana Cloud direct Loki:**
Use your numeric org ID as `username` and a service account token as `api_key`.
The `url` should be your Grafana instance URL. miru will auto-detect and proxy through it.

**Self-hosted Loki (no auth):**
Set `url` to your Loki URL and omit `api_key`. miru detects direct Loki automatically.

### 2. Create the config file

```bash
mkdir -p ~/.config/miru
cp config.example.toml ~/.config/miru/config.toml
```

Edit `~/.config/miru/config.toml`:

```toml
[grafana]
url = "https://grafana.yourcompany.com"
api_key = "glsa_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"

[loki]
service_label = "app"      # the Loki label that identifies services
default_limit = 200        # lines returned when not specified
max_limit = 1000           # hard cap, model cannot exceed this

# Optional: add a metrics backend (requires Grafana). Omit for logs-only.
# [prometheus]
# datasource = "Prometheus"   # defaults to the first Prometheus datasource
```

The `service_label` key varies by setup. To run metrics-only, omit `[loki]` and keep `[prometheus]`.

### Keeping the token out of the config file

Setting the `MIRU_API_KEY` environment variable overrides `api_key` from the
file, so you can keep the token out of the file entirely. This is the
recommended way to supply the secret. Leave `api_key` unset and let your MCP
host pass it through its `env` block:

```json
{
  "mcpServers": {
    "miru": {
      "command": "miru",
      "args": [],
      "env": { "MIRU_API_KEY": "glsa_xxxxxxxxxxxxxxxxxxxxxxxx" }
    }
  }
}
```

Some hosts (for example Gemini CLI) expand `$VARS`, so you can reference a
value from your shell or a secret manager instead of pasting the token:
`"MIRU_API_KEY": "$GRAFANA_TOKEN"`.

If you do keep the token in the config file, restrict it to your user:

```bash
chmod 600 ~/.config/miru/config.toml
```

### 3. Add to your MCP host

miru is a standard stdio MCP server: the host runs the `miru` binary and talks to it over stdin/stdout. Most hosts use the same JSON block; Codex uses TOML.

JSON hosts (Claude Code via `.mcp.json` or `claude mcp add`, Gemini CLI via `~/.gemini/settings.json`):

```json
{
  "mcpServers": {
    "miru": {
      "command": "miru",
      "args": []
    }
  }
}
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.miru]
command = "miru"
args = []
```

If `miru` is not on your PATH, use the full path, e.g. `/home/you/.local/bin/miru`.

To use a non-default config location, set `MIRU_CONFIG` in the server's environment:

```json
{
  "mcpServers": {
    "miru": {
      "command": "miru",
      "args": [],
      "env": { "MIRU_CONFIG": "/path/to/your/config.toml" }
    }
  }
}
```

```toml
[mcp_servers.miru]
command = "miru"
args = []

[mcp_servers.miru.env]
MIRU_CONFIG = "/path/to/your/config.toml"
```

## Tools

| Tool            | Description                                                                                             |
| --------------- | ------------------------------------------------------------------------------------------------------- |
| `list_services` | Lists all services in Loki. Call this first. (logs)                                                     |
| `query_logs`    | Fetches log lines from a service. Optional: `level` (any level name your logs use, e.g. error, warn, crit), `search` (text or regex). (logs) |
| `list_metrics`  | Lists Prometheus metric names with their type (counter/gauge/histogram) and help. Call this first. Optional: `filter` substring. (metrics) |
| `query_metrics` | Runs a PromQL query. Returns a per-series summary plus data points over time, or current values with `instant=true`. (metrics) |

Metric tools return an error if `[prometheus]` is not configured; log tools error if `[loki]` is not configured.

## Example prompts

```
What services are available in Loki?
```

```
Show me the last 50 error logs from the auth service in the past hour.
```

```
Show me warnings from api-gateway in the last hour.
```

```
Search for "connection refused" errors in the db-proxy service in the last 30 minutes.
```

```
What errors is the payment service throwing right now? (last 15 minutes)
```

```
Compare error rates between the auth and db-proxy services over the last 30 minutes.
```

```
Which pods had a CPU spike in the last hour?
```

```
What's the current memory usage per instance of the api service?
```

```
Show me the 95th percentile request latency for the checkout service over the last 3 hours.
```

## Configuration reference

| Key                    | Required | Default | Description                                                               |
|------------------------|----------|---------|---------------------------------------------------------------------------|
| `grafana.url`          | yes      |         | Grafana instance URL or direct Loki URL. No trailing slash.               |
| `grafana.api_key`      | no       |         | Service account token for Bearer auth, or password when `username` is set. Overridden by the `MIRU_API_KEY` environment variable. |
| `grafana.username`     | no       |         | Enables Basic auth. Set to your Grafana Cloud org ID for direct Loki.     |
| `grafana.datasource`   | no       |         | Deprecated alias for `loki.datasource`.                                   |
| `loki`                 | no\*     |         | Logs backend. Omit for metrics-only.                                       |
| `loki.service_label`   | yes      |         | Loki label key that identifies services. Required when `[loki]` is set.    |
| `loki.level_label`     | no       |         | Loki label for log level. Enables label-selector filtering when set.      |
| `loki.datasource`      | no       |         | Grafana Loki datasource name. Defaults to the first Loki datasource found. |
| `loki.default_limit`   | no       | `200`   | Default max log lines per query                                           |
| `loki.max_limit`       | no       | `1000`  | Hard cap. Model cannot request more than this.                            |
| `prometheus`           | no\*     |         | Metrics backend. Omit for logs-only. Requires Grafana.                     |
| `prometheus.datasource`| no       |         | Grafana Prometheus datasource name. Defaults to the first one found.       |
| `prometheus.target_points` | no   | `100`   | Target data points per series; miru derives the step from the time window. |
| `prometheus.max_series`| no       | `20`    | Max series returned before miru asks the model to aggregate in PromQL.    |
| `prometheus.min_step_seconds` | no | `15`   | Smallest step miru will compute for a range query.                        |

\* At least one of `[loki]` or `[prometheus]` is required.

Config file location: `~/.config/miru/config.toml`
Override with: `MIRU_CONFIG=/path/to/config.toml`

Environment variables:
- `MIRU_API_KEY`: overrides `grafana.api_key`
- `MIRU_CONFIG`: path to the config file

## License

Licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0
([LICENSE-APACHE](LICENSE-APACHE)) at your option.
