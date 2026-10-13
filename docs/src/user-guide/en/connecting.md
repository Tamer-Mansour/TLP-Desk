# Connecting

Use the **Connect to a device** card on the home screen of *your* PC (the viewer).

## 1. Say which PC

Type one of these in the **ID, name or IP address** field:

| You type | Example | Notes |
|---|---|---|
| The **device ID** | `123 456 789` | Recommended. Spaces and dashes are optional. TLP-Desk checks that the PC that answers really owns this ID. |
| The **device name** | `Office-PC` | Looked up among devices on the network; upper/lower case does not matter. |
| The **IP address** | `192.168.1.40` | Works even when discovery is blocked. Add `:port` only if the other PC uses a different port. |

Or simply click **Connect** next to a device under **Devices on this network** — that connects by
its ID.

You can type the ID or the address with Arabic-Indic digits (`١٢٣ ٤٥٦ ٧٨٩`); TLP-Desk converts them.

## 2. Choose how to be let in

The second field, **One-time code (optional)**, decides how the other PC lets you in:

- **Leave it empty** → the person at the other PC sees an **Accept** prompt and decides. Use this
  when someone is sitting there.
- **Type the 6-digit code** shown on the other PC → you are let in without a prompt. Use this when
  the other person read the code to you, or when you are at both PCs.

## 3. Press Connect

Press **Connect** (or Enter). A window shows **Connecting to …** and, once the other PC shows its
prompt, **Waiting for the other side to accept…**. Click **Cancel** to stop; the prompt on the other
PC disappears.

When the connection is made, the other PC's screen opens in your window. See
[Controlling the remote PC](controlling.md).

## If it does not work

A **Could not connect** message tells you why:

| Message | Meaning | What to do |
|---|---|---|
| Device not found on this network. | The ID or name was not seen by discovery within a few seconds. | Check TLP-Desk is open there; try the IP address instead. |
| The device cannot be reached. | No answer from that address. | Same network? Firewall? See [Troubleshooting](troubleshooting.md). |
| The connection was declined. | The other person clicked **Decline**, or did not answer within 60 seconds. | Ask them to accept, or use the one-time code. |
| Wrong code or password. | The code was wrong or has already been used. | Ask for the current code — it changes after each use. |
| Too many attempts. Try again later. | Too many wrong codes or prompts from your PC. | Wait 10 minutes. |
| This device does not accept connections from you. | The other PC does not allow this way of connecting (for example its code login is turned off). | Leave the code empty to ask for Accept. |
| The other device runs an incompatible version. | The two PCs run TLP-Desk versions that cannot talk to each other. | Install the same version on both. |
| Only addresses on the local network can be used. | You typed an internet address. | TLP-Desk works on your local network only. |
| Enter a 9-digit ID, a device name or an IP address. | The text is not a valid ID, name or address. | Check for typos. |
| The one-time code has 6 digits. | The code field has the wrong length. | Type all 6 digits, or leave it empty. |
| Identity check failed. Connection stopped. | The PC that answered is **not** the one with that ID, or it could not prove it knows the code. | Do not retry blindly — see [Security and privacy](security-privacy.md). |

If you see **Warning: this device's identity has changed**, read
[When the host key has changed](host-key-changed.md).
