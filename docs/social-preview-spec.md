# Social Preview Image Specification

GitHub recommends: 1280×640px, < 1MB

## Design

Background: #141414 (very dark charcoal)

Left side (60% width):
- Title: "CYDRUST" — large, bold, cyberpunk font (Orbitron or similar)
  - Color gradient: #D97757 → #A78BFA (left to right)
- Subtitle: "Real-time AI Session Monitor" — #888888, smaller
- Tagline: "ESP32 • Rust • Claude Code" — #4ADE80, monospace font
- Three stat badges:
  - 🟢 3 Working   (green)
  - 🟡 1 Waiting   (amber)
  - ⚫ 2 Idle      (gray)

Right side (40% width):
- ESP32 display mockup (rounded rectangle, dark border)
- Show SESSIONS tab active
- 2-3 session cards rendered in the mock display
- Slight glow effect matching #D97757

Tools to create:
- Figma, Sketch, or GIMP
- Or: generate with carbon.now.sh + screenshot
- Or: use GitHub's og-image service style

Export as: docs/assets/banner.png
