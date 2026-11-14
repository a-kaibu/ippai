#!/bin/bash
# Writes colors.wmv, the ASF movie the mf unit tests read through the Media Foundation source:
# WMV2 64x48 at 24 fps, 18 frames (6 red, 6 lime, 6 blue), a key frame every 6, and WMA2 stereo
# 440 Hz for 0.75 s. Run again only to change it.
set -euo pipefail
cd "$(dirname "$0")"
ffmpeg -y -v error -f lavfi -i "color=c=red:s=64x48:r=24:d=0.25[a];color=c=lime:s=64x48:r=24:d=0.25[b];color=c=blue:s=64x48:r=24:d=0.25[c];[a][b][c]concat=n=3:v=1:a=0" \
  -f lavfi -i "sine=f=440:r=44100:d=0.75" -map 0:v -map 1:a -c:v wmv2 -g 6 -b:v 200k -c:a wmav2 -b:a 64k -ac 2 \
  -map_metadata -1 -fflags +bitexact -flags +bitexact colors.wmv
