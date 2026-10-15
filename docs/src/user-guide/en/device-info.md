# Remote device information

See the other PC's details without leaving the session: its name, Windows version, processor,
memory, disks with free space, and how long it has been running.

## Open the panel

1. In the session toolbar click **Device info**.
2. The panel asks the other PC and fills in a few seconds later (or right away on a fast network).
3. Click **Refresh** to ask again, for example to see the disk space after copying files.

The panel shows:

| Line | Meaning |
|---|---|
| Computer name | The name of the remote PC |
| System / Version | Windows edition and build |
| Processor / Threads | CPU model and number of logical processors |
| Memory | Installed RAM |
| Running for | Time since the PC started |
| Disks | One bar per drive: used space, and "x free of y" |

Sizes and numbers follow your digit setting (Arabic-Indic digits and Arabic units when the
interface is Arabic). Names that contain Arabic are shown correctly, even when mixed with English.

If the other PC does not answer within ten seconds the panel says so; it probably runs an older
TLP-Desk without this feature.

## Permission

Anyone who can **view** the screen can read this information, so it needs no extra permission.
The panel only shows hardware and system facts; it never shows files, accounts, passwords or the
network address.

## From the command line

```powershell
TLP-Desk-cli info 123456789 --code 042917
TLP-Desk-cli info 123456789 --code 042917 --json
```

The `--json` form prints a single object with `type` set to `device_info`.
