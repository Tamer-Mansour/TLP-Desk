# Sound from the remote PC

Hear what the remote PC plays (music, video, notifications) through your own speakers during a
session. Only the sound the PC sends to its speakers is shared; the host's microphone is never
used.

## Permission

Sound needs the **Hear the sound** permission, which the person at the host grants. It is **off by
default**: tick **Hear the sound** under Settings > Incoming connections on the host (the starting
permissions of the next session), or switch it on for a running session from the host's
permission list. They can take it back at any moment: the sound stops at once and the speaker
button on your side shows that the other device does not share its sound.

## Using it

1. Connect as usual. If the host allows sound and **Play the remote computer's sound when
   connecting** is on (Settings > Sound, on by default), you hear it right away.
2. The session toolbar has a **speaker** button. Click it to mute or play the remote sound.
3. Next to it is a **volume** slider. It only changes the volume on your PC; the remote PC's own
   volume is not touched.

If your PC has no output device, a message says that the remote sound cannot be played.

## How it works

- The host captures the sound only while a viewer wants it (zero work otherwise) and sends
  nothing while the PC is silent.
- Any speaker setup is converted to stereo, compressed with Opus in 10 ms pieces and sent over
  the session's encrypted connection.
- Your PC keeps a very small buffer (about 30 ms, never more than 60 ms). A packet that arrives
  too late is skipped and one that is lost is concealed, so the delay never grows: a short
  glitch is preferred to sound that drifts behind the picture.

## Good to know

- The sound of other sessions is not mixed in: each viewer chooses for itself.
- Session recordings do not contain sound yet.
- Everything stays on your local network; nothing is stored.
