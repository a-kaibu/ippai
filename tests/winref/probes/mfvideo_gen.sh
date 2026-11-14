#!/bin/bash
# Writes mfvideo_data.h: the movie the mfvideo probe plays with Media Foundation, made with FFmpeg:
# H.264 Main 64x48 at 24 fps, 18 frames (6 red, 6 lime, 6 blue), B-frames (2) and a key frame
# every 6 (presentation order differs from decoding order; the edit list starts it at 0), moov
# first. Run again only to change it (the bytes are what Windows' answers were recorded from).
set -euo pipefail
cd "$(dirname "$0")"
T=$(mktemp -d)
ffmpeg -v error -f lavfi -i "color=c=red:s=64x48:r=24:d=0.25[a];color=c=lime:s=64x48:r=24:d=0.25[b];color=c=blue:s=64x48:r=24:d=0.25[c];[a][b][c]concat=n=3:v=1:a=0" \
  -c:v libx264 -profile:v main -bf 2 -g 6 -pix_fmt yuv420p -map_metadata -1 -fflags +bitexact -flags:v +bitexact -movflags +faststart "$T/a.mp4"
{
  echo "/* mfvideo_gen.sh */"
  echo "static const unsigned char file_mp4[] = {"
  od -An -v -tx1 "$T/a.mp4" | sed 's/ \([0-9a-f][0-9a-f]\)/0x\1,/g'
  echo "};"
} > mfvideo_data.h
rm -r "$T"
