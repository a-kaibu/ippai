#!/bin/bash
# Writes acmmp3_data.h: the MP3s the acmmp3 probe decodes, made with FFmpeg's libmp3lame (no ID3
# tag, no Xing frame: what a WAVE's MP3 data holds). Run again only to change the set (the bytes
# are what Windows' answers were recorded from).
set -euo pipefail
cd "$(dirname "$0")"
T=$(mktemp -d)
# 0.5 s: 44.1 kHz stereo (440 Hz left, 660 Hz right) at 64 kbps, MPEG-1; 22.05 kHz mono 550 Hz at
# 32 kbps, MPEG-2
ffmpeg -v error -f lavfi -i "aevalsrc=0.5*sin(2*PI*440*t)|0.25*sin(2*PI*660*t):s=44100:d=0.5" -c:a libmp3lame -b:a 64k -write_xing 0 -id3v2_version 0 -f mp3 "$T/s.mp3"
ffmpeg -v error -f lavfi -i "aevalsrc=0.5*sin(2*PI*550*t):s=22050:d=0.5" -c:a libmp3lame -b:a 32k -write_xing 0 -id3v2_version 0 -f mp3 "$T/m.mp3"
{
  echo "/* acmmp3_gen.sh */"
  for n in s m; do
    echo "static const unsigned char mp3_$n[] = {"
    od -An -v -tx1 "$T/$n.mp3" | sed 's/ \([0-9a-f][0-9a-f]\)/0x\1,/g'
    echo "};"
  done
} > acmmp3_data.h
rm -r "$T"
