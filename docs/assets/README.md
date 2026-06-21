# Assets

Place these files here:

- `banner.png` — 1280×640px social preview banner
- `demo.gif` — Screen recording of the display in action
- `hardware-photo.jpg` — Photo of assembled hardware
- `wiring-diagram.png` — Wiring diagram image

## Banner Design Spec

- Size: 1280×640px (GitHub social preview ratio)
- Background: `#141414` (dark)
- Title: "CYDRUST" in large cyberpunk font
- Subtitle: "Real-time AI Session Monitor"
- Right side: ESP32 display mockup showing session cards
- Colors: `#D97757` (Claude orange), `#A78BFA` (Codex purple)

## Demo GIF Spec

Record a 10–15 second clip showing:

1. A Claude session starting (card appears in `working` state, `>>` indicator)
2. Claude stopping (card transitions to `waiting` / `!` amber indicator)
3. Tap on the USAGE tab to show the usage bar view
4. Tap back to SESSIONS

Recommended tools:
- OBS Studio for screen capture of the bridge state, cross-faded with phone footage
  of the physical display
- FFMPEG to convert to optimised GIF: `ffmpeg -i demo.mp4 -vf "fps=10,scale=640:-1" demo.gif`

## Hardware Photo Spec

- Landscape orientation, display visible and in focus
- Show USB cable connected
- Good ambient lighting — avoid flash glare on the display glass
- Preferred backdrop: dark surface or the `#141414` colour swatch

## Wiring Diagram Spec

Create with Fritzing or draw.io. Export as 1920×1080 PNG.

Show:
- ESP32 DevKit v1 (or CYD board)
- ST7789 display module
- All eight wires colour-coded per the wiring table in `docs/hardware.md`
- Pin labels on both ends
- A small legend in the corner listing the signal names
