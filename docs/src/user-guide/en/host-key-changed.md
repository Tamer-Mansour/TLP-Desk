# When the host key has changed

Sometimes, when you connect to a PC you have connected to before, TLP-Desk stops and shows:

```text
┌──────────────────────────────────────────────────────────────────┐
│ Warning: this device's identity has changed                       │
│                                                                   │
│ 192.168.1.40 presented a different key than in earlier            │
│ connections. Someone may be intercepting the connection, or the   │
│ device was reinstalled. Continue only if you are sure it was      │
│ reinstalled.                                                      │
│                                                                   │
│   Remembered key   3fa1 09c2 77de 4b10                            │
│   Presented key    a8e4 5d21 0c9b f337                            │
│                                                                   │
│                     [Cancel]  [Forget old key and connect]        │
└──────────────────────────────────────────────────────────────────┘
```

## What it means

Every TLP-Desk installation has its own secret **key**. The first time you connect to a PC, your
TLP-Desk remembers that PC's key. Every later time, it checks that the PC presents the **same**
key. This warning means it did not.

Nothing has been sent to the other PC yet: the person there has not seen any request, and the
other side has not received your code.

There are two possible reasons:

| Harmless | Dangerous |
|---|---|
| The other PC was **reinstalled** (Windows reinstalled, or TLP-Desk's data folder `%ProgramData%\TLP-Desk` deleted), so it made a new key. Its device ID has changed too. | Another device on your network is **pretending** to be that PC to watch what you do or capture what you type. |
| A **different PC** now has that IP address (for example the router gave the address to another computer). | |

## What to do

1. Click **Cancel** first.
2. Find out whether the other PC was reinstalled or replaced. Ask the person who uses it, or look
   at it: is its **device ID** the same as before? A reinstalled PC shows a new ID.
3. Prefer connecting **by device ID** (shown on the other PC's home screen) rather than by IP
   address. If you dial the ID shown on that PC and TLP-Desk connects without a warning, you have
   reached the right PC.
4. Only if you are sure the change is legitimate, connect again and click **Forget old key and
   connect**. TLP-Desk then remembers the new key.

If you cannot explain the change — especially on a network shared with people you do not know —
do not continue.

## Related messages

- **Identity check failed. Connection stopped.** — you dialed a device ID and the PC that answered
  is not the owner of that ID (or it failed to prove it knew the one-time code). TLP-Desk refuses
  outright; there is nothing to "forget". Check the ID and the network.

See [Security and privacy](security-privacy.md) for how identities work.
