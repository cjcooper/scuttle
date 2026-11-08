#!/usr/bin/env python3
"""Generates the menu bar (tray) icons: a monochrome template container with a
status badge in the corner. macOS tints template icons for light and dark
menu bars, so state is shown by shape, not colour.

  running     filled dot
  stopped     hollow ring
  busy-a/b    ring with the top / bottom half filled (alternated while
              starting, stopping or restarting)
  error       ring with a dash (engine unreachable, profile missing, ...)

Regenerate (from the repo root):
  python3 src-tauri/icons/make_tray_icon.py src-tauri/icons/tray
  for f in src-tauri/icons/tray/*.svg; do
    rsvg-convert -w 44 -h 44 "$f" -o "${f%.svg}.png"
  done
"""
import os, sys

BX, BY, BR = 50, 50, 11          # badge centre and radius (64x64 canvas)

CUBE = '''<g fill="none" stroke="#000" stroke-width="5" stroke-linejoin="round" stroke-linecap="round" transform="translate(-2,-3)">
    <path d="M30 6 L52 18 L52 44 L30 56 L8 44 L8 18 Z"/>
    <path d="M8 18 L30 30 L52 18 M30 30 L30 56"/>
    <path d="M16.5 28 L16.5 42 M22.5 31.5 L22.5 46 M37.5 31.5 L37.5 46 M43.5 28 L43.5 42" stroke-width="3.5"/>
  </g>'''

BADGES = {
    "running": f'<circle cx="{BX}" cy="{BY}" r="{BR}" fill="#000"/>',
    "stopped": f'<circle cx="{BX}" cy="{BY}" r="{BR-2.5}" fill="none" stroke="#000" stroke-width="5"/>',
    "busy-a": (f'<circle cx="{BX}" cy="{BY}" r="{BR-2.5}" fill="none" stroke="#000" stroke-width="5"/>'
               f'<path d="M{BX-BR} {BY} A{BR} {BR} 0 0 1 {BX+BR} {BY} Z" fill="#000"/>'),
    "busy-b": (f'<circle cx="{BX}" cy="{BY}" r="{BR-2.5}" fill="none" stroke="#000" stroke-width="5"/>'
               f'<path d="M{BX+BR} {BY} A{BR} {BR} 0 0 1 {BX-BR} {BY} Z" fill="#000"/>'),
    "error": (f'<circle cx="{BX}" cy="{BY}" r="{BR-2.5}" fill="none" stroke="#000" stroke-width="5"/>'
              f'<line x1="{BX-5}" y1="{BY}" x2="{BX+5}" y2="{BY}" stroke="#000" stroke-width="4.5" stroke-linecap="round"/>'),
}

def svg(badge):
    # The cube is cut away around the badge so the two read separately.
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <defs>
    <mask id="cut">
      <rect width="64" height="64" fill="#fff"/>
      <circle cx="{BX}" cy="{BY}" r="{BR+4}" fill="#000"/>
    </mask>
  </defs>
  <g mask="url(#cut)">
  {CUBE}
  </g>
  {badge}
</svg>
'''

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
for name, badge in BADGES.items():
    with open(os.path.join(out, f"{name}.svg"), "w") as f:
        f.write(svg(badge))
