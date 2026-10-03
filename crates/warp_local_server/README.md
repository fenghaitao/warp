# Warp local agent server

A standalone compatibility server for Warp's `/ai/multi-agent` endpoint. It
translates Warp Protobuf requests to OpenAI-compatible chat completions (DeepSeek
by default), then returns URL-safe base64 Protobuf events over SSE. It makes no
calls to Warp services. Tools execute in the client, through its permission checks.

## Build and run

From the repository root, in PowerShell:

```powershell
cargo build -p warp_local_server
# Set DEEPSEEK_API_KEY in this terminal through your usual secret-management flow.
.\target\debug\warp_local_server.exe --model deepseek-v4-pro
```

The default address is `http://127.0.0.1:8080`. `GET /health` returns `ok`.
`--bind 127.0.0.1:8082` changes the port; non-loopback addresses are rejected.

The provider URL is fixed at server startup. To use another OpenAI-compatible
provider or force a particular model:

```powershell
.\target\debug\warp_local_server.exe --base-url https://api.deepseek.com/v1 --model deepseek-v4-pro
```

`WARP_LOCAL_PROVIDER_URL` and `WARP_LOCAL_MODEL` are equivalent environment
variables. Without a model override, the server resolves the client's selected
`config_key` to the custom endpoint's model slug. Without a selection, it uses
`deepseek-chat`. Use a model actually available to your provider account.

`DEEPSEEK_API_KEY` takes precedence over the custom endpoint key carried in the
Warp request. The client endpoint URL must match the configured server provider
URL. Keys are never accepted on the command line or written to server logs.

## Supported behavior

- User queries, tool-result follow-ups, and resume with complete client history.
- Streaming text and reasoning, including fragmented function names/arguments.
- `run_shell_command`, `read_files`, `grep`, `file_glob_v2`, `apply_file_diffs`.
- The client's supported-tools list and create-files capability are honored.
- One tool call per model turn; parallel calls are disabled. Reasoning is shown
  only when the client supports it, otherwise preserved as opaque message data.
- Tool IDs and result types are checked; invalid batches emit no tool calls.
- Shell commands use conservative risk flags and wait for completion. The model
  cannot mark them as automatically approved. The server never executes commands.
- Original function arguments and reasoning survive a tool round trip, including
  DeepSeek's `reasoning_content` history requirement.
- Disconnecting cancels upstream inference. Four requests may run concurrently;
  requests and provider streams are limited to 16 MiB, with provider timeouts.
- Provider authentication failures, rate limits, truncated streams, and malformed
  responses return terminal Warp events without exposing provider error bodies.

The TUI owns conversation history and round-trips it in each request. The server
needs no database; restarting it preserves conversations if the client supplies
their history. Resuming an outstanding tool call requires its result or a
cancellation result; the server does not rerun commands automatically.

This initial version rejects subtasks, summarization, skill invocation, referenced
attachments, and image context. It does not implement MCP tools, web tools,
long-running command interaction, cloud sync, account APIs, or model discovery.
Large conversations currently require a fresh session rather than automatic
compaction.

## Run the TUI without a Warp login

Build the TUI and start the server in a separate PowerShell terminal:

```powershell
cargo build -p warp_tui --bin warp-tui-oss --features standalone
.\target\debug\warp_local_server.exe --model deepseek-v4-pro
```

Then launch the TUI:

```powershell
.\target\debug\warp-tui-oss.exe --local-agent-url http://127.0.0.1:8080
```

`WARP_LOCAL_AGENT_URL` is equivalent to the flag. The URL must be an HTTP loopback
origin, with no credentials, path, query, or fragment. It conflicts with
`--api-key`/`WARP_API_KEY` and cloud `--resume`; unset `WARP_API_KEY` in the local
TUI terminal if necessary.

Local mode sends only `/ai/multi-agent` inference to this origin, omitting Warp
authentication, ambient/team headers, IAP headers, HTTP proxies, and redirects.
It creates a temporary client identity, starts the terminal without device
login, and denies authenticated Warp service requests. It bypasses Warp credit
checks and disables passive suggestions, autoupdate, cloud conversation storage,
and telemetry. Saved privacy settings and Warp credentials are preserved.

The TUI keeps its existing settings, permissions, tool execution, and local
history. A server `--model` override determines the actual provider model;
otherwise select a configured custom endpoint model in the TUI. Cloud features
and unsupported server tools remain unavailable. This server provides no fake
account or subscription responses. Without the local-agent flag or environment
variable, normal Warp login and hosted routing apply.

## Verify

```powershell
cargo nextest run -p warp_local_server --no-fail-fast
cargo clippy -p warp_local_server --all-targets --tests -- -D warnings
```

HTTP tests use a local mock provider and decode returned events with Warp's
generated Protobuf types. They exercise Unicode/SSE chunk boundaries, reasoning,
tool argument assembly, follow-up history, provider errors, unsupported requests,
and client-disconnect cancellation. They require no API keys or Warp account.
