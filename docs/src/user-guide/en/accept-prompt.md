# The Accept prompt and the connection banner

![The Accept window on the host](../../media/accept-popup-en-light.png)

This page is about the **host** — the PC someone wants to connect to.

## The Accept window

When someone connects without your one-time code, a **Connection request** window appears in the
bottom-right corner of your screen, above all other windows. It appears even when TLP-Desk is
minimized, hidden in the notification area, or not open at all (when TLP-Desk runs as a service,
it starts itself quietly in the notification area to ask you). You never have to open TLP-Desk to
answer. A short Windows notification sound plays when it appears.

The window shows:

- the other PC's **name**, a round badge with its initials, its **ID** and its local **address**.
  The ID is verified: TLP-Desk checked cryptographically that the other PC owns this ID. The name
  is whatever the other PC calls itself — anyone can choose any name. If you are not sure, compare
  the ID with the one shown on that person's screen;
- a chip saying how they asked (**Accept prompt**);
- **Allow this device to:** one chip per permission (see the screen, use the mouse, use the
  keyboard, share the clipboard, transfer files, hear the sound, chat, ...). The chips that are
  on come from **Settings > Incoming connections**. Click a chip to switch it off (or on) for this
  connection only. Seeing the screen is what a session is, so it cannot be switched off;
- **Trust this device**: remembered on this PC for later. It does not skip this question yet;
- a ring around the badge that empties while time runs out, and *Declines automatically in N s*.

Click **Accept** to let them in with the permissions that are on, or **Decline** to refuse
(**Esc** also declines). To prevent accidents, the buttons only react a moment after the window
appears, and **Enter never accepts**. If you do nothing until the countdown ends, the request is
declined automatically. If the other person cancels first, the window disappears.

You can still take any permission back during the session.

Prefer the old behaviour? Untick **Show connection requests in their own window** in
**Settings > Incoming connections**: the same card then appears inside the TLP-Desk window. The
sound has its own switch next to it. If the window cannot be shown for any reason, TLP-Desk falls
back to asking inside its main window.

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

## Several people at once

Two or more colleagues can be connected and in control at the same time. Each one has their own
picture (they can even watch different monitors), their own permissions and their own pointer
marker; one leaving does not disturb the others. While more than one person is connected, each
line of the banner has two extra switches:

- **View only** takes the mouse and keyboard away from that person. They still see the screen.
  Untick it to give control back.
- **Pause input** ignores that person's mouse and keyboard for now without changing what they are
  allowed to do; they see the same "the other person is using the PC" message. Untick it to let
  them continue.

Your own mouse and keyboard still win over everyone.

## Long sessions and network hiccups

Nothing ends a session by itself: no time limit, no idle timeout. If the network drops for a
moment, the session simply continues. If it stays down for longer (more than about 15 seconds),
the viewer reconnects on its own for up to two minutes, **without a new Accept**, and with exactly
the permissions you allow at that moment. Ending a session yourself (End session, View permission
removed, closing TLP-Desk) always ends it for good: that person needs a new Accept to come back.

To stop the session for good, click **End session** — or simply close TLP-Desk.
