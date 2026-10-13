# tlp-desk-cli

**The headless binary `TLP-Desk-cli.exe`** for scripts, tests and AI agents. It uses the same
`Host` and `Viewer` as the GUI, so anything it can do the GUI can talk to, and vice versa.

| | |
|---|---|
| Path | `crates/tlp-desk-cli` (binary `TLP-Desk-cli`) |
| Depends on | `proto`, `crypto`, `net`, `host`, `viewer`, `clap` 4 (derive), `png`, `tokio`, `tracing-subscriber`, `anyhow` |
| Unsafe | `#![forbid(unsafe_code)]` |
| Strings | every user-facing string lives in `src/text.rs` (English only for now; `--lang` is on the roadmap) |

## Global options

| Option | Meaning |
|---|---|
| `--data-dir <PATH>` | Identity and known-hosts directory (default: the same as the GUI) |
| `-v`, `-vv` | Log level: warn (default), info, debug — to **stderr**. `RUST_LOG` overrides |

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

> Neither `serve` nor the GUI configures a password yet, so `--password` is currently refused with
> "not allowed". It exists for the unattended-access work in M3.

## Examples

```powershell
# Host on one PC (accept on the console, or allow the one-time code)
TLP-Desk-cli serve --code

# Viewer on another PC: connect by ID with the code, save a screenshot
TLP-Desk-cli connect "123 456 789" --code 042917 --seconds 5 --snapshot shot.png

# What is on the network?
TLP-Desk-cli list --seconds 3
```

Snapshots are for local testing; never commit them (see `.gitignore`).

## Roadmap

M4 lists `list`, `connect`, `send-file` and `exec-wol` for scripts and agents; `send-file` and
`exec-wol` are not implemented yet. The CLI has no tests of its own; its building blocks are
tested in `host`, `viewer` and `net`.
