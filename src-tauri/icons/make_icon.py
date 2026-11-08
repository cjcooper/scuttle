#!/usr/bin/env python3
"""Generates the app icon sources: a light and a dark version.

  icon.svg        light, the default everywhere (Windows .exe, Linux, macOS before 26)
  icon-dark.svg   dark, for the window icon (Windows) and About window when dark
  AppIcon.icon/   Icon Composer document for macOS 26+, which shows the light or
                  dark version (and the clear and tinted ones) as the system asks

Regenerate the icons with (from the repo root):
  python3 src-tauri/icons/make_icon.py src-tauri/icons
  rsvg-convert -w 1024 -h 1024 src-tauri/icons/icon.svg -o src-tauri/icons/app-icon.png
  cargo tauri icon src-tauri/icons/app-icon.png -o src-tauri/icons
  rsvg-convert -w 256 -h 256 src-tauri/icons/icon.svg -o src-tauri/icons/256x256.png
  rsvg-convert -w 48 -h 48 src-tauri/icons/icon.svg -o src-tauri/icons/48x48.png
  rsvg-convert -w 256 -h 256 src-tauri/icons/icon-dark.svg -o src-tauri/icons/dark-256x256.png
  cp src-tauri/icons/128x128@2x.png ui/icon.png
  cp src-tauri/icons/dark-256x256.png ui/icon-dark.png

Preview the macOS versions with Icon Composer's ictool (Xcode 26+), e.g.:
  "$(xcode-select -p)/../Applications/Icon Composer.app/Contents/Executables/ictool" \\
    src-tauri/icons/AppIcon.icon --export-image --output-file dark.png --platform macOS \\
    --rendition Dark --width 1024 --height 1024 --scale 1

Design: a container (an isometric box with corrugated sides) set in a control
dial, the same family as Subduer's icon. The amber lid and the amber arc show
the dial turned to "running". Light is a blue box on a white tile, dark a
light-blue box on a charcoal tile; the palettes match Subduer's.
"""
import json, math, os, sys
C = 512

START, SWEEP, LEVEL = 135.0, 270.0, 0.62      # dial: gap at the bottom, 62% turned
POINTER = START + SWEEP*LEVEL

# Per version: tile gradient (top, bottom), box, amber, the lid's inner colour,
# and the colour and opacity of the faint hexagon and the dial track.
PALETTES = {
    "light": dict(bg=("#ffffff", "#dce5f2"), box="#1f5bd6", amber="#f29d0c", face="#ffffff",
                  hexa=("#1f5bd6", 0.08), track=("#1f5bd6", 0.16), edge=0.10),
    "dark": dict(bg=("#2b3445", "#0d1119"), box="#6fa8ff", amber="#ffb21e", face="#141a25",
                 hexa=("#6fa8ff", 0.10), track=("#ffffff", 0.14), edge=0.0),
}

def pt(r, deg, cx=C, cy=C):
    a = math.radians(deg)
    return cx + r*math.cos(a), cy + r*math.sin(a)

def arc(r, a0, a1):
    x0, y0 = pt(r, a0); x1, y1 = pt(r, a1)
    large = 1 if (a1-a0) > 180 else 0
    return f"M{x0:.1f},{y0:.1f} A{r},{r} 0 {large} 1 {x1:.1f},{y1:.1f}"

def poly(points):
    return "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y in points) + " Z"

def dial(p):
    """The back layer: a faint hexagon (the box's silhouette) and the dial."""
    hexa = [pt(300, -90 + i*60) for i in range(6)]
    hc, ho = p["hexa"]; tc, to = p["track"]
    return [
        f'<g opacity="{ho}"><path d="{poly(hexa)}" fill="{hc}" stroke="{hc}" stroke-width="60" stroke-linejoin="round"/></g>',
        f'<path d="{arc(352, START, START+SWEEP)}" fill="none" stroke="{tc}" stroke-opacity="{to}" stroke-width="22" stroke-linecap="round"/>',
        f'<path d="{arc(352, START, POINTER)}" fill="none" stroke="{p["amber"]}" stroke-width="22" stroke-linecap="round"/>',
    ]

def box(p):
    """The front layer: an isometric container with an amber lid and
    corrugated sides."""
    R, cy = 222, C + 14                       # box "radius", nudged down to sit in the dial
    top, ur, lr, bot, ll, ul = (pt(R, -90 + i*60, C, cy) for i in range(6))
    mid = (C, cy)
    lid = [top, ur, mid, ul]
    left = [ul, mid, bot, ll]
    right = [mid, ur, lr, bot]
    stroke = f'stroke="{p["box"]}" stroke-width="40" stroke-linejoin="round" stroke-linecap="round"'
    parts = [
        f'<path d="{poly(left)}" fill="{p["face"]}" fill-opacity="0.0" {stroke}/>',
        f'<path d="{poly(right)}" fill="{p["face"]}" fill-opacity="0.0" {stroke}/>',
    ]
    # Corrugation: vertical ridges on both sides.
    def ridges(a, b, c, d):
        """Ridges across the face a-b (top edge) / d-c (bottom edge)."""
        out = []
        for t in (0.27, 0.5, 0.73):
            x0 = a[0] + (b[0]-a[0])*t; y0 = a[1] + (b[1]-a[1])*t
            x1 = d[0] + (c[0]-d[0])*t; y1 = d[1] + (c[1]-d[1])*t
            out.append(f'<line x1="{x0:.1f}" y1="{y0+44:.1f}" x2="{x1:.1f}" y2="{y1-44:.1f}" stroke="{p["box"]}" stroke-width="26" stroke-linecap="round"/>')
        return out
    parts += ridges(ul, mid, bot, ll) + ridges(mid, ur, lr, bot)
    # The lid, in amber, drawn last so it reads as on top.
    parts.append(f'<path d="{poly(lid)}" fill="{p["amber"]}" stroke="{p["amber"]}" stroke-width="40" stroke-linejoin="round"/>')
    return parts

def svg(body, defs=""):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">\n'
            + (f"  <defs>\n{defs}  </defs>\n" if defs else "")
            + "".join(f"  {line}\n" for line in body) + "</svg>\n")

def flat(p):
    """The whole icon on a rounded tile, with the macOS grid's margin and shadow."""
    top, bottom = p["bg"]
    defs = f'''    <linearGradient id="bg" x1="0" y1="0" x2="0.6" y2="1">
      <stop offset="0" stop-color="{top}"/>
      <stop offset="1" stop-color="{bottom}"/>
    </linearGradient>
    <linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.14"/>
      <stop offset="0.5" stop-color="#ffffff" stop-opacity="0"/>
    </linearGradient>
    <filter id="shadow" x="-10%" y="-10%" width="120%" height="125%">
      <feDropShadow dx="0" dy="12" stdDeviation="14" flood-color="#000" flood-opacity="0.28"/>
    </filter>
'''
    tile = 'x="100" y="100" width="824" height="824" rx="185"'
    body = [f'<g filter="url(#shadow)"><rect {tile} fill="url(#bg)"/></g>',
            f'<rect {tile} fill="url(#sheen)"/>']
    # A hairline keeps the white tile from melting into light backgrounds.
    if p["edge"]:
        body.append(f'<rect x="102" y="102" width="820" height="820" rx="183" fill="none" stroke="#0b1f5c" stroke-opacity="{p["edge"]}" stroke-width="4"/>')
    body += dial(p) + box(p)
    return svg(body, defs)

def srgb(hex_color):
    r, g, b = (int(hex_color[i:i+2], 16)/255 for i in (1, 3, 5))
    return f"srgb:{r:.5f},{g:.5f},{b:.5f},1.00000"

def icon_composer(out):
    """AppIcon.icon: the system draws the tile and the glass; we give it the
    tile colours and two layers per version."""
    assets = os.path.join(out, "AppIcon.icon", "Assets")
    os.makedirs(assets, exist_ok=True)
    for mode, p in PALETTES.items():
        for name, layer in (("dial", dial), ("box", box)):
            with open(os.path.join(assets, f"{name}-{mode}.svg"), "w") as f:
                f.write(svg(layer(p)))
    def gradient(mode):
        return {"linear-gradient": [srgb(c) for c in PALETTES[mode]["bg"]], "orientation": {"start": {"x": 0.5, "y": 0}, "stop": {"x": 0.5, "y": 1}}}
    def group(name):
        return {"layers": [{
                    "name": name,
                    "image-name-specializations": [
                        {"value": f"{name}-light.svg"},
                        {"appearance": "dark", "value": f"{name}-dark.svg"},
                    ],
                    "glass": True,
                }],
                "shadow": {"kind": "neutral", "opacity": 0.5},
                "translucency": {"enabled": True, "value": 0.4}}
    doc = {
        "fill-specializations": [
            {"value": gradient("light")},
            {"appearance": "dark", "value": gradient("dark")},
        ],
        "groups": [group("box"), group("dial")],   # front first
        "supported-platforms": {"squares": ["macOS"]},
    }
    with open(os.path.join(out, "AppIcon.icon", "icon.json"), "w") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")

out = sys.argv[1]
with open(os.path.join(out, "icon.svg"), "w") as f:
    f.write(flat(PALETTES["light"]))
with open(os.path.join(out, "icon-dark.svg"), "w") as f:
    f.write(flat(PALETTES["dark"]))
icon_composer(out)
