# Session recording

![The session toolbar while recording: red dot and elapsed time](../../media/recording-en-light.png)

Record what you see during a session to a video file on your own PC, then play it back later in
TLP-Desk or in any video player.


## Recording

1. During a session, click the **Record** button (a circle) in the session toolbar.
2. A red dot and the elapsed time (for example "Recording 12:34") appear next to the button for as
   long as the recording runs.
3. Click the button again to stop. The file is closed and appears in the **Recordings** list.

TLP-Desk stores the video exactly as the remote PC sends it, without re-encoding it, so recording
costs almost no extra CPU. The result is a standard MP4 (H.264) file that Windows Media Player,
VLC and web browsers play. Sound is not recorded yet.

Files go to `%USERPROFILE%\Videos\TLP-Desk\` and are named
`TLP-Desk <device name> <date time>.mp4`. Arabic device names are kept exactly as they are.

## Good to know

- **Only you see the recording.** It happens on your PC from the video the host already sends. The
  host is not told and sees no banner about it. Tell the person at the other PC if you record, and
  record only where you are allowed to.
- **The remote screen size changes** (the host switched monitors or changed resolution): the
  recording continues in a new file, named like the first with " (2)", " (3)" and so on.
- **Disk space:** TLP-Desk does not start recording when less than 256 MB is free, and stops
  cleanly when the disk is nearly full. You are told why.
- **If the PC or TLP-Desk crashes** while recording, everything except the last second still plays.
  In the Recordings list such a file is marked **Interrupted**; click **Repair** to finish it so
  every player can open it.

## The Recordings list

On the home screen click **Recordings**. For each recording you can:

- **Play** it in TLP-Desk,
- **Open folder** to see the files in Windows Explorer,
- **Delete** it (you are asked first; deleting is permanent),
- **Repair** an interrupted one.

## The playback window

- **Play / Pause** (or the Space key) and a **timeline** you can click or drag. Seeking jumps to the
  nearest earlier key picture, so it lands within a few seconds of where you clicked.
- The left and right arrow keys step 5 seconds backward and forward.
- **Speed** from 0.5x to 4x.
- **Fit to window** or **Actual size**.

Playback works the same in the Arabic and English interface; the controls follow the interface
direction, and the video itself is never mirrored.
