# Port forwarding (tunnels)

Reach a network service on or near the remote PC through the session — for example open a web
page served by the remote PC in your own browser — without changing any router settings.

## Allow it (on the remote PC)

Port forwarding is **off** for every new session. While someone is connected, the banner at the
top shows **Allow port forwarding**: tick it to allow, untick it to stop. Unticking closes every
forwarded connection at once. The banner also shows how many forwarded connections are open.

The remote PC only lets tunnels reach itself and devices on its own local network, never the
internet.

## Forward a port (on your PC)

1. In the session toolbar click **Port forwarding**.
2. Enter **Port on this PC** (e.g. `8080`), **Remote address** (empty or `127.0.0.1` means the
   remote PC itself; or a LAN address such as `192.168.1.10`) and **Remote port** (e.g. `80`).
3. Click **Start forwarding**. Now `http://127.0.0.1:8080` on your PC opens the remote web
   server.

Each rule shows its open and total connections and the bytes sent and received; **Stop** ends
it. By default the port only accepts connections from your own PC. **Let other devices on my
network use this port** opens it to your whole network — only do this on a network you trust.

## From the command line

```text
TLP-Desk-cli serve --allow-tunnels                       (remote PC)
TLP-Desk-cli forward 192.168.1.20 8080:127.0.0.1:80      (your PC, until Ctrl+C)
```
