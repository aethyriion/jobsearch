#!/usr/bin/env bash
# Build the social-media clip: docs/demo-social.mp4
#
#   ./docs/demo-data.sh && ./docs/demo-social.sh
#
# Why this exists separately from docs/demo.gif:
#
# The README's GIF is 209 columns at font size 14. GitHub renders it around
# 880px wide with click-to-enlarge, so it reads fine there. A social feed
# renders media at roughly 552px, which puts that recording's effective font
# at about 4px — a grey smear that reads as carelessness.
#
# Two changes fix it. A bigger font with fewer columns, and a crop down to
# the list pane: at any width where the font is legible in a feed, the 64/36
# split leaves the list too narrow to show company and title, which are the
# only columns that make the clip mean anything. Recording wide and cropping
# gets both.
#
# MP4 rather than GIF because feeds autoplay and loop video, allow
# tap-to-fullscreen, and compress it far better than hundreds of GIF frames.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v vhs >/dev/null || { echo "vhs is not installed: https://github.com/charmbracelet/vhs"; exit 1; }
command -v ffmpeg >/dev/null || { echo "ffmpeg is not installed"; exit 1; }

RAW=$(mktemp -d)/raw.mp4
vhs docs/demo-social.tape
mv docs/demo-social.mp4 "$RAW"

# 0.638 is the list pane plus its border, with the detail pane cut off
# cleanly. If the 64/36 split in src/tui/ui.rs changes, this changes too.
ffmpeg -v error -i "$RAW" \
  -vf "crop=iw*0.638:ih:0:0" \
  -c:v libx264 -crf 20 -pix_fmt yuv420p -movflags +faststart \
  docs/demo-social.mp4 -y

rm -f "$RAW"
printf 'docs/demo-social.mp4  %s  %s\n' \
  "$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0:s=x docs/demo-social.mp4)" \
  "$(du -h docs/demo-social.mp4 | cut -f1)"
