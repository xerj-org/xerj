#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="$(dirname "$here")"
chrome="${CHROME:-google-chrome}"
W=410
H=660
for card in 1 2; do
  for theme in light dark; do
    png="$out/card-$card-$theme.png"
    "$chrome" --headless=new --hide-scrollbars --disable-gpu \
      --default-background-color=00000000 --force-device-scale-factor=2 \
      --window-size=$W,$H --virtual-time-budget=10000 \
      --screenshot="$png" "file://$here/card.html?card=$card&theme=$theme" 2>/dev/null
    python3 - "$png" <<'PY'
import sys
from PIL import Image
p = sys.argv[1]
im = Image.open(p).convert("RGBA")
im.quantize(colors=256, method=Image.Quantize.LIBIMAGEQUANT, dither=Image.Dither.FLOYDSTEINBERG).save(p, optimize=True)
PY
    echo "$png $(stat -c %s "$png") bytes"
  done
done
