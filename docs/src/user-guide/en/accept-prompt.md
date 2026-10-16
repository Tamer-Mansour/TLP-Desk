# The Accept prompt and the connection banner

![The Accept prompt on the host](../../media/incoming-en-light.png)

This page is about the **host** — the PC someone wants to connect to.

## The Accept prompt

When someone connects without your one-time code, TLP-Desk comes to the front and shows:

```text
┌──────────────────────────────────────────────┐
│ Incoming connection                          │
│                                              │
│ Lab-Laptop wants to view and control this    │
│ device.                                      │
│                                              │
│   Name     Lab-Laptop                        │
│   ID       735 120 664                       │
│   Address  192.168.1.81                      │
│                                              │
│                        [Decline]  [Accept]   │
└──────────────────────────────────────────────┘
```

- **ID** is verified: TLP-Desk checked cryptographically that the other PC owns this ID.
- **Name** is whatever the other PC calls itself — anyone can choose any name. If you are not
  sure, compare the ID with the one shown on that person's screen.
- **Address** is the other PC's local IP address.

Click **Accept** to let them in, or **Decline** to refuse. If you do nothing for 60 seconds, the
request is declined automatically. If the other person cancels first, the prompt disappears.

**What Accept allows:** the other person can **see your screen and use your mouse and keyboard**.
Nothing else — no files, no clipboard, no audio (those features are not available yet, and will need
their own permission).

Only accept people you know and expect. If prompts keep appearing from a PC you do not recognise,
decline them; after more than 5 requests in a minute TLP-Desk blocks that PC for 10 minutes.

## The "who is connected" banner

As long as anyone is connected to your PC, a coloured bar stays at the top of the TLP-Desk window:

```text
┌────────────────────────────────────────────────────────────┐
│ Lab-Laptop is connected to this device      [End session]  │
└────────────────────────────────────────────────────────────┘
```

- There is one line per connected person.
- **End session** disconnects that person immediately.
- The banner is shown on every screen of TLP-Desk, even while you are yourself connected to another
  PC.
- TLP-Desk also comes to the front when a session starts, so a connection never goes unnoticed.
- If you minimise TLP-Desk or send it to the notification area, a small always-on-top banner with
  the same text and an **End session** button stays on screen while anyone is connected. It does
  not take keyboard focus when it appears. It is visible to the viewer too (it is deliberately not
  hidden from screen capture), so they can see that you see them.

## While someone controls your PC

- A **green pointer** with their name follows the mouse whenever they move it, so you can see that
  the movement is remote and who it is. It is not visible to them.
- **You always win:** as soon as you move your own mouse or press a key, their control pauses, and
  resumes about one second after you stop. Anything they were holding down (a key, a mouse button)
  is released. They see a message that you are using the PC.

To stop the session for good, click **End session** — or simply close TLP-Desk.
