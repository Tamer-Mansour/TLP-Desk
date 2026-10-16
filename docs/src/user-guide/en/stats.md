# The statistics overlay

![Session with the statistics overlay](../../media/session-en-light.png)

The **Stats** button in the session toolbar (a pulse-line icon) shows or hides a small panel over the picture (top
right; top left in Arabic). It is on by default and updates once a second.

| Line | Meaning | Typical on a good LAN |
|---|---|---|
| **Frame rate** | Pictures per second you receive and display (fps) | up to 60 fps while the screen changes; close to 0 when nothing moves |
| **Latency** | Delay from a change on the remote screen to the moment the picture is drawn here (ms); shown with a `~` while it is still a rough estimate | 5–30 ms wired, more on Wi-Fi |
| **Bitrate** | Network data used for the picture (Mbps) | a few Mbps; up to about 15 Mbps for fast full-screen motion |
| **Resolution** | Size of the picture being sent, then `Q≈` and a number: how finely it is compressed (lower is sharper) | the remote monitor's size, at most 1920 × 1200; `Q≈` 15 to 28 |
| **Duration** | How long the session has lasted | |

## Reading the numbers

- A **low frame rate while nothing moves is normal**: TLP-Desk sends a new picture only when the
  screen changes.
- **Latency** is measured with small probes: your PC asks the remote PC for its clock twice a
  second, works out the offset from the fastest answer (so a slow reply cannot skew it), and then
  compares the moment the remote PC captured each picture with the moment this one is drawn. It
  does not include your monitor's own display time. Until the first probe returns, a rough estimate
  is used and the number is prefixed with `~`.
- **Bitrate adapts by itself.** The remote PC lowers the bitrate (and, if needed, the picture size or
  frame rate) when the network is congested and raises it again slowly when things are calm, so latency
  does not build up. When the remote screen does not change, nothing is sent at all, so the bitrate
  drops to almost zero while idle and rises while you type or scroll.
- **The picture gets smaller before it gets blurry.** When the network cannot carry the full
  picture, the remote PC sends a smaller one (for example 1600 × 1000, then 1280 × 800) so text
  stays sharp, and returns to the full size after about ten calm seconds. The **Resolution** line
  shows the size in use; it changes at most every few seconds, and the picture is scaled to fill
  your window. The number after `Q≈` stays low (below about 30) on purpose; it is not shown for
  some graphics cards.
- If the frame rate drops and latency stays low, TLP-Desk is skipping pictures on purpose to keep
  the delay short — it never lets a backlog build up. A busy Wi-Fi or a slow PC on either side is
  the usual cause.
- If latency climbs above about 100 ms, see
  [Troubleshooting](troubleshooting.md#the-picture-is-slow-or-jerky).
