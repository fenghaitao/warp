# Local Warp TUI verification

Verified on Windows on 2026-10-03. The first local inference path works with the
headless `warp-tui-oss` client and DeepSeek, without a Warp login.

## Live check

The server ran with:

```powershell
.\target\debug\warp_local_server.exe --bind 127.0.0.1:8082 --base-url https://api.deepseek.com/v1 --model deepseek-v4-pro
```

The provider key came from the existing environment; it was not put on the
command line or captured in this report. The existing settings also use
`https://api.deepseek.com/v1`.

The newly built client ran in a real Windows PTY with an isolated profile:

```powershell
$env:WARP_DATA_PROFILE='local-agent-verify-20261003'
.\target\debug\warp-tui-oss.exe --local-agent-url http://127.0.0.1:8082
```

It displayed `Warp Agent CLI` and `Signed in as Local agent`, reached its input
view, and did not show a Warp login prompt. The submitted prompt was:

> Use run_shell_command to execute exactly echo WARP_LOCAL_TOOL_OK_20261003,
> then report the printed marker. Do not read or modify files.

Observed terminal excerpts:

```text
Is it OK if I run this command and read the output?
echo WARP_LOCAL_TOOL_OK_20261003
(1) yes
(2) no

✓ Ran `echo WARP_LOCAL_TOOL_OK_20261003`

The printed marker is:
WARP_LOCAL_TOOL_OK_20261003
```

Auto-approve stayed off. Only this command was approved. DeepSeek streamed
reasoning, requested the tool, and completed its response after the client
executed the command and supplied the result.

A connection snapshot for the new TUI process (PID 36008) showed its established
connection to `127.0.0.1:8082`. The server used the machine's local proxy for its
provider connection. This snapshot is supporting evidence, not an all-time
network trace. Both verification processes were stopped afterward; the TUI
exited successfully with no cloud resume recommendation.

The executable was built successfully in 27 minutes; its timestamp was
2026-10-03 15:27:44. `--help` exposes `--local-agent-url`.

## Automated checks

```powershell
cargo nextest run --offline -p warp_multi_agent_client -p warp_server_client --lib --no-fail-fast
cargo nextest run --offline -p warp -p warp_tui --lib --features warp_tui/standalone -E 'test(local_agent) | test(local_tui) | test(session::tests) | test(lib_tests) | test(tui::tests) | test(settings::privacy)' --no-fail-fast
```

- Client libraries: 63 passed; run ID `d9732c95-4596-4c07-873d-152f176067ae`.
- Focused app/TUI checks: 94 passed; run ID `aa7c16ff-1f22-42cc-a558-532e4d4f44ef`.
  These are targeted tests; 7,795 unrelated tests were skipped.
- The unchanged server implementation previously passed 20 tests and its scoped
  Clippy check. Its live health endpoint also returned `ok` in this verification.

Client Clippy passed with warnings denied:

```powershell
cargo clippy --offline --no-deps -p warp_multi_agent_client -p warp_server_client -p warp_server_auth -p http_client --all-targets --tests -- -D warnings
```

App/TUI Clippy passed with the existing `unnecessary_unwrap` lint exempted and
all other warnings denied:

```powershell
cargo clippy --offline --no-deps -p warp -p warp_tui --lib --features warp_tui/standalone -- -D warnings -A clippy::unnecessary_unwrap
```

The strict app lint attempt found four existing instances in unchanged files:
`conversation_usage_view.rs:367`, `next_command_model.rs:807`,
`block_list_element.rs:1979`, and `presence_manager.rs:652`. The initial check
that also linted dependencies found two more existing instances in GUI
`hoverable.rs:722` and `hoverable.rs:735`. Those files were left unchanged.

## Scope

This establishes login-free TUI startup and a real DeepSeek shell-tool round
trip on Windows. MCP, web tools, subtasks, attachment/image context, interactive
commands, and automatic compaction remain unsupported by the server. Other
operating systems were not runtime-tested because cloud runner tooling was
unavailable. See `crates/warp_local_server/README.md` for supported behavior and
startup instructions.
