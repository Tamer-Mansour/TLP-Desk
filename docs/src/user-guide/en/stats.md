# The statistics overlay

![Session with the statistics overlay](../../media/session-en-light.png)

The **Stats** button in the session toolbar (a pulse-line icon) shows or hides a small panel over the picture (top
right; top left in Arabic). It is on by default and updates once a second.

| Line | Meaning | Typical on a good LAN |
|---|---|---|
| **Frame rate** | Pictures per second you receive and display (fps) | up to 60 fps while the screen changes; close to 0 when nothing moves |
| **Latency** | Estimated delay from a change on the remote screen to the decoded picture here (ms) | 5–30 ms wired, more on Wi-Fi |
| **Bitrate** | Network data used for the picture (Mbps) | a few Mbps; up to about 15 Mbps for fast full-screen motion |
| **Resolution** | Size of the picture being sent | the remote monitor's size, at most 1920 × 1200 |
| **Duration** | How long the session has lasted | |

## Reading the numbers

- A **low frame rate while nothing moves is normal**: TLP-Desk sends a new picture only when the
  screen changes.
- **Latency** is an estimate. It does not include the time your own monitor needs to show the
  picture, so the real delay is a little higher.
- If the frame rate drops and latency stays low, TLP-Desk is skipping pictures on purpose to keep
  the delay short — it never lets a backlog build up. A busy Wi-Fi or a slow PC on either side is
  the usual cause.
- If latency climbs above about 100 ms, see
  [Troubleshooting](troubleshooting.md#the-picture-is-slow-or-jerky).
