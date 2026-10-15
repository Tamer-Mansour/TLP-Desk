# Whiteboard: draw on the remote screen

![The viewer in drawing mode: tools bar, a red circle and a blue arrow on the remote screen](../../media/whiteboard-en-light.png)

Point things out by drawing on the other PC's screen. The person at that PC sees your pen, circles
and arrows on top of their desktop while you talk them through a problem. It needs the
**Whiteboard** permission, which sessions get by default; the person at the host can take it away
at any moment.

## Drawing

1. During a session, click the **Draw** button (a pen) in the session toolbar. A tools bar opens
   under the toolbar.
2. Drag on the remote screen to draw. While drawing mode is on, the mouse **draws instead of
   controlling** the remote PC: no clicks or key presses reach it. Click the button again to go
   back to controlling.
3. The person at the host sees every stroke while you are still drawing it.

The tools bar has:

- **Tools:** pen, highlighter (wide and see-through), arrow, rectangle and eraser.
- **Colors** and three **line widths**.
- **Fade out:** drawings disappear a few seconds after you finish them, so you do not have to clean
  up.
- **Undo**, **Redo** and **Clear all**.

Drawings belong to the screen you were looking at. If you switch to another monitor they are
cleared on both sides.

## On the host

The strokes appear in a click-through layer above everything else on the shared screen: the
person at the host can keep working underneath. A small round **clear** button in the corner of the
screen wipes all drawings at once. Your viewer is told and shows "The other side cleared the
drawings".

The drawings disappear by themselves when the session ends or when the Whiteboard permission is
revoked. TLP-Desk does not store them.

## Good to know

- **Only the person who is allowed to see the screen can draw on it.** If the host does not grant
  the permission, the Draw button is disabled and the tooltip says so.
- **The drawing layer is not part of the video.** TLP-Desk draws the same strokes again on your
  side on top of the picture, so they show instantly and stay sharp instead of waiting for a
  captured, compressed frame. A screen recording made on the host does not contain them.
- **Limits protect the host:** a drawing holds at most 256 strokes, and very long strokes are
  simplified before they are sent.
- Drawing works the same in the Arabic and English interface; the tools bar follows the interface
  direction and the picture itself is never mirrored.
