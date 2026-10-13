# Your device ID and one-time code

The **This device** card on the home screen shows what someone else needs in order to connect to
your PC.

## Device ID

A 9-digit number such as `482 913 057`.

- It is **permanent** for this PC: it does not change when you restart TLP-Desk or Windows, or when
  you update or reinstall TLP-Desk.
- It is **calculated from this PC's secret identity key**. That is why it is safe to connect by ID:
  another device cannot simply pretend to have your ID — TLP-Desk checks the key behind it.
- Click **Copy** next to it to put it on the clipboard. It is always copied with Western digits
  (`0-9`), even if you display Arabic-Indic digits, so it pastes correctly everywhere.

Your ID changes only if the identity key is deleted — for example when Windows is reinstalled, or
if you delete `%ProgramData%\TLP-Desk` on purpose.

## One-time code

A 6-digit code such as `042 917`.

- Give it to someone you trust when you want them to connect **without** you having to click
  **Accept**. They type it in the **One-time code** field when connecting.
- It works **once**: after a successful connection, a new code appears automatically.
- It also changes after 3 wrong attempts, and you can change it yourself at any time with
  **New code**.
- The code is never sent over the network, not even encrypted: both PCs only prove to each other
  that they know it.

> Share the code only with someone you trust. Anyone on your network who knows it can connect
> once — and will be able to see and control your PC.

If you prefer to approve every connection yourself, simply don't share the code: when the other
person leaves the code field empty, you get an [Accept prompt](accept-prompt.md).

## Name

Your PC's name as other TLP-Desk users see it in their device list. By default it is the Windows
computer name. You can change it in [Settings](settings.md#device-name).

## Local address

Your PC's IP address(es) on the local network, for example `192.168.1.23`, each with a **Copy**
button. Someone can type this address to connect when automatic discovery does not work. Only
local addresses are shown (`10.x.x.x`, `172.16–31.x.x`, `192.168.x.x`, link-local and private IPv6);
TLP-Desk never uses internet addresses.
