# tlp-desk-cli

**The headless binary `TLP-Desk-cli.exe`** for scripts, tests and AI agents. It uses the same
`Host` and `Viewer` as the GUI, so anything it can do the GUI can talk to, and vice versa.

| | |
|---|---|
| Path | `crates/tlp-desk-cli` (binary `TLP-Desk-cli`) |
| Depends on | `proto`, `crypto`, `net`, `host`, `viewer`, `clap` 4 (derive, env), `serde_json`, `png`, `tokio`, `tracing-subscriber`, `anyhow` |
| Unsafe | `#![forbid(unsafe_code)]` |
| Strings | every user-facing string lives in `src/text.rs`, in English and Arabic (`--lang`) |

## Global options

| Option | Meaning |
|---|---|
| `--data-dir <PATH>` | Identity and known-hosts directory (default: the same as the GUI) |
| `-v`, `-vv` | Log level: warn (default), info, debug — to **stderr**. `RUST_LOG` overrides |
| `--json` | Machine-readable output: stdout carries only JSON Lines (one object per line, each with a `"type"`); progress lines move to stderr |
| `-q`, `--quiet` | No progress lines; results and errors only |
| `--lang en\|ar` | Language of human-readable output (env `TLP_DESK_LANG`). JSON is never translated. IDs, codes, addresses and numbers stay ASCII in both languages |

Passwords are never accepted on the command line (other local users could read them from the
process list): `--password` is a flag that reads `TLP_DESK_PASSWORD`.

Arabic on the console: Rust's standard library writes to a Windows console with `WriteConsoleW`, so
Arabic arrives intact whatever the console code page (no `chcp 65001` needed); piped output is UTF-8.
Windows Terminal joins and orders Arabic letters correctly; the legacy console window shows them
unjoined.

## Exit codes

Stable: scripts may branch on them. Also printed at the end of `--help`.

| Code | Name (`kind` in JSON) | Meaning |
|---|---|---|
| 0 | `ok` | the command did what was asked |
| 1 | `failure` | any other error (I/O, bad file, internal, session ended early) |
| 2 | `usage` | bad command line (from clap) |
| 3 | `not_found` | device ID / name / WoL alias not found |
| 4 | `unreachable` | host unreachable, or outside the LAN allow-list |
| 5 | `auth_rejected` | rejected, wrong code/password, or locked out |
| 6 | `key_mismatch` | host key does not match the dialed ID / remembered key (possible MITM) |
| 7 | `permission_denied` | the session lacks the permission the command needs |
| 8 | `timeout` | no answer in time |
| 9 | `version_mismatch` | the peer speaks another protocol version |
| 130 | `interrupted` | Ctrl+C |

With `--json` a failure also prints `{"type":"error","code":8,"kind":"timeout","message":"..."}`
on stdout.

## JSON output

Every object has a `"type"`. Device IDs are 9 ASCII digits without spaces.

| Command | Objects |
|---|---|
| `id` | `identity {device_id, fingerprint, data_dir}` |
| `list` | `devices {devices: [{device_id, name, addrs, protocol_version, via}]}` |
| `connect` | `connected {peer, permissions}`, `monitors {active, monitors}`, `stats {t_s, fps, bitrate_kbps, rtt_ms, latency_ms, decode_ms, width, height, frames_decoded, frames_received}` (every second), `permissions`, `host_input_priority {local_active}`, `ended {reason}`, `summary {frames_decoded, seconds, avg_fps}`, `snapshot {written, path?, width?, height?}` |
| `serve` | `listening {device_id, fingerprint, name, addr}`, `code {code}`, `incoming {peer, auth, auto_accept}`, `session_started {session, peer, auth, permissions}`, `session_ended`, `auth_failed`, `request_cancelled`, `device_info_served {session}` |
| `info` | `device_info {os, version, cpu, cores, ram_bytes, disks: [{mount, label, total_bytes, free_bytes}], uptime_s, hostname, peer}` |
| `wol` | `wol_sent {mac, to}` |

`peer` is `{device_id, name, addr}`; `permissions` is a list of `view`, `mouse`, `keyboard`,
`clipboard`, `files`, `audio`, `restart`, `privacy`, `chat`, `tunnel`, `whiteboard`, `system`.

## Subcommands

### `id`

Prints this device's ID, full fingerprint and key directory.

### `list [--seconds N]`

Discovers devices for N seconds (default 5) and prints ID, name, addresses, protocol version and
whether each came from mDNS or broadcast.

### `serve`

Runs a host until Ctrl+C.

| Option | Default | Meaning |
|---|---|---|
| `--port` | 47400 | UDP port |
| `--fps` | 60 | frame rate cap |
| `--monitor` | 0 | monitor to stream |
| `--view-only` | off | grant only `VIEW` |
| `--code` | off | **enable** one-time-code access (off by default in the CLI) and print the code |
| `--auto-accept` | off | accept every viewer that asks, without a prompt — testing only, prints a warning |
| `--no-discovery` | off | do not advertise on the LAN |
| `--name <NAME>` | computer name | advertised name |
| `--local-grace-ms` | 1000 | local input priority grace window; 0 disables it |
| `--no-pointer-marker` | off | do not show the green remote-pointer marker |

Without `--auto-accept`, each Accept request is shown on the console: `Accept? [y/N]` — only `y`,
`Y` or `yes` accepts.

### `connect <TARGET>`

`TARGET` is a 9-digit device ID, `IP[:port]`, `ID@IP[:port]` or a device name.

| Option | Meaning |
|---|---|
| `--code <CODE>` | authenticate with the host's one-time code (otherwise the host gets an Accept prompt) |
| `--password` | authenticate with the password in the `TLP_DESK_PASSWORD` environment variable |
| `--seconds N` | stay connected N seconds (default 10) |
| `--snapshot <PNG>` | save the last decoded frame |
| `--monitor <IDX>` | switch to that host monitor |
| `--move-pointer` | move the remote pointer in a small circle (input path smoke test) |
| `--forget-key` | forget a stored host key first (after a deliberate reinstall) |
| `--port N` | local UDP port (default 0 = any) |

It prints a stats line every second, host input-priority changes and the monitor list, and a
summary at the end. If the host key changed it prints a loud warning and tells you to re-run with
`--forget-key` **only** if you know the host was reinstalled.

`serve` loads the permanent password stored by the app or by `set-password` (only its Argon2id
verifier), so `--password` works against it; without one the host refuses it.

### `set-password [--remove]`

Sets the permanent unattended-access password from the `TLP_DESK_PASSWORD` environment variable
(never argv), refusing passwords below the minimum length, or removes it with `--remove`. Writes
the verifier into the data dir (`--data-dir`), which needs administrator rights for
`%ProgramData%\TLP-Desk`.

### `info <TARGET>`

Connects like `connect` (same `--code`, `--password`, `--port`, `--forget-key`), asks the host for
its device info and prints OS, version, CPU and threads, RAM, uptime and each disk's free/total
space, then disconnects. Needs the `VIEW` permission on the host (exit 7 without it).
`--timeout N` (default 10 s) bounds the wait for the answer (exit 8).

### `wol <MAC|ALIAS>` (alias `exec-wol`)

Sends a Wake-on-LAN magic packet to every IPv4 subnet broadcast (port 9), or with
`--to <IP[:port]>` to one LAN address (addresses outside the LAN allow-list are refused). An alias is
looked up in `wol-aliases.txt` in the data directory: one `name = MAC` per line, UTF-8 (Arabic names
work), `#` starts a comment. Unknown alias: exit 3.

### Commands from other lanes

| Command | Needs | Notes |
|---|---|---|
| `chat <TARGET> <MESSAGE> [--wait N]` | `CHAT` | sends one message, prints replies for N seconds |
| `clip-send <TARGET> [TEXT] [--file F]` | `CLIPBOARD` | text to the host clipboard; `serve --clipboard` is the receiving side |
| `ls`, `mkdir`, `rm [-r]`, `mv` | `FILES` | remote file system; `serve --allow-files` |
| `send-file`, `get-file` | `FILES` | transfers with progress lines; `--to` is the destination |
| `forward <TARGET> <LOCAL:HOST:REMOTE>...` | `TUNNEL` | runs until Ctrl+C; `serve --allow-tunnels` |

`--help` is bilingual: with `--lang ar` (or `TLP_DESK_LANG=ar`) the command summaries and the
exit-code table are Arabic. The Arabic summaries live in `commands/help.rs` (`ARABIC`); a test
fails when a subcommand has no row, so every new command must add one. On Windows the console
is switched to UTF-8 at start (`console.rs`).

## Adding a command (other lanes)

One file per subcommand in `src/commands/<name>.rs` with a clap `Args` struct and
`run(ctx, args)`; then one line at each `FEATURE HOOK` in `src/commands/mod.rs` (`mod`, the `Cmd`
variant, the `run` arm). Shared helpers live in `src/commands/common.rs`: `DialArgs`/`AuthArgs`
(flatten them for the standard target/auth flags), `dial(ctx, &args, features)`, `peer_json`,
`id_str`. Print through `ctx.out` (`progress`, `event`, `result`) so `--json` and `--quiet` work, put
strings in `text.rs` with an Arabic translation, and return `CliError` variants for failures that
have their own exit code. Host-side features for `serve` are registered in
`serve::host_features()`.

## Examples

```powershell
# Host on one PC (accept on the console, or allow the one-time code)
TLP-Desk-cli serve --code

# Viewer on another PC: connect by ID with the code, save a screenshot
TLP-Desk-cli connect "123 456 789" --code 042917 --seconds 5 --snapshot shot.png

# What is on the network? (JSON for scripts)
TLP-Desk-cli list --seconds 3 --json

# Remote device info, in Arabic
TLP-Desk-cli --lang ar info 123456789 --code 042917

# Wake a PC by alias, then wait for it
TLP-Desk-cli wol "مكتب المحاسبة"
```

Snapshots are for local testing; never commit them (see `.gitignore`).

## Roadmap

M4 lists `list`, `connect`, `send-file` and `exec-wol` for scripts and agents; `wol`/`exec-wol` and
`info` are implemented, `send-file`, `get-file`, `ls`, `forward` and `chat` come from their lanes.
Tests: `cargo test -p tlp-desk-cli` (argument parsing with `try_parse_from`, JSON schemas, exit
codes, Arabic text); device info end to end is `tlp-desk-host` e2e
`device_info_request_is_answered_and_cached`.
