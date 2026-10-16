# Command line

For shortcuts and scripts.

## Starting the app with options

```powershell
"C:\Program Files\TLP-Desk\TLP-Desk.exe" --connect 192.168.1.40 --code 042917
```

| Option | What it does |
|---|---|
| `--connect <ID, name or IP>` | Starts connecting right away |
| `--code <6 digits>` | Uses this one-time code for `--connect` (without it, the other side gets an Accept prompt) |
| `--lang ar` / `--lang en` | Starts in Arabic or English |
| `--settings` | Opens the Settings screen first |
| `--port <number>` | Uses another UDP port instead of 47400 (other PCs must then add `:port` to your address) |

Tip: create a desktop shortcut to `TLP-Desk.exe` and add `--connect 123456789` to its **Target** to
connect to a PC you use often with one double-click.

## The command-line tool

`TLP-Desk-cli.exe` (installed next to the app) runs without a window. It is meant for scripts, tests
and automation.

```powershell
TLP-Desk-cli --help                 # all commands
TLP-Desk-cli id                     # this PC's device ID and key fingerprint
TLP-Desk-cli list                   # devices on the network (5 seconds)
TLP-Desk-cli serve --code           # be a host; print a one-time code
TLP-Desk-cli connect 123456789 --code 042917 --seconds 10 --snapshot shot.png
TLP-Desk-cli info 123456789 --code 042917   # the other PC's system, CPU, memory, disks, uptime
TLP-Desk-cli wol 01:23:45:67:89:ab          # wake a sleeping PC (Wake-on-LAN)
TLP-Desk-cli list --json                    # machine-readable output for scripts
TLP-Desk-cli chat 123456789 "مرحباً، hello" --code 042917 --wait 10   # send a chat message, print replies
TLP-Desk-cli ls 123456789 --code 042917 D:\Shared   # also: mkdir, rm, mv (need the Files permission)
```

## All commands

| Command | What it does |
|---|---|
| `id` | This PC's device ID and key fingerprint |
| `list [--json]` | Devices found on the network |
| `serve` | Be a host (`--code`, `--allow-files`, `--allow-tunnels`, `--clipboard`, `--view-only` ...) |
| `connect <target>` | Connect as a viewer; print statistics, optional `--snapshot` |
| `info <target>` | The other PC's name, system, CPU, memory, disks and uptime |
| `set-password [--remove]` | Set (from `TLP_DESK_PASSWORD`) or remove the permanent [unattended-access](unattended-access.md) password |
| `wol <MAC or alias>` (also `exec-wol`) | Wake a sleeping PC with Wake-on-LAN |
| `chat <target> <message>` | Send one chat message and print the replies |
| `clip-send <target> <text>` | Put text on the other PC's clipboard |
| `ls`, `mkdir`, `rm`, `mv` `<target> <path>` | Browse and change folders on the other PC (Files permission) |
| `send-file`, `get-file <target>` | Copy files and folders to or from the other PC |
| `forward <target> <local:host:remote>` | Forward TCP ports until Ctrl+C |

`<target>` is a device ID, an IP address or a device name. Every command accepts `--code <6
digits>` or `--password` (see below), `--json`, `-q`, `-v` and `--lang`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Done |
| 1 | Any other error |
| 2 | Bad command line |
| 3 | Device or alias not found |
| 4 | Host unreachable |
| 5 | Rejected, wrong code or password, or locked out |
| 6 | Host key does not match (possible attack) |
| 7 | The session lacks the needed permission |
| 8 | No answer in time |
| 9 | The other side runs another protocol version |
| 130 | Stopped with Ctrl+C |

When `serve` receives a request without a code, it asks on the console: **Accept? [y/N]**.

Notes:

- Add `--lang ar` (or set `TLP_DESK_LANG=ar`) for Arabic messages; Windows Terminal shows Arabic best.
- `--json` prints one JSON object per line; `-q` hides progress lines. The exit code tells scripts
  what went wrong (table at the end of `TLP-Desk-cli --help`).
- For a password, set `TLP_DESK_PASSWORD` and pass `--password`; the password is never typed on the
  command line itself.
- Only one program can listen on port 47400 at a time; if TLP-Desk is open, run `serve` with
  `--port 47410`, for example.
- Full reference for developers: [tlp-desk-cli](../../dev/crates/cli.md).
