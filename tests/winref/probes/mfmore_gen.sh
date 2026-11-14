#!/bin/bash
# Writes mfmore_data.h: the sound files the mfmore probe opens with the Source Reader, made with
# FFmpeg (the tone of mfreader_gen.sh: 440 Hz left at 0.5, 660 Hz right at 0.25): a FLAC
# (22.05 kHz, 0.4 s), a WMA 2 in ASF (44.1 kHz, 64 kb/s, 0.5 s), an Ogg Vorbis (44.1 kHz, 0.5 s).
# Run again only to change the set (the bytes are what Windows' answers were recorded from).
set -euo pipefail
cd "$(dirname "$0")"
T=$(mktemp -d)
S="aevalsrc=0.5*sin(2*PI*440*t)|0.25*sin(2*PI*660*t)"
ffmpeg -v error -f lavfi -i "$S:s=22050:d=0.4" -c:a flac -map_metadata -1 -fflags +bitexact "$T/a.flac"
ffmpeg -v error -f lavfi -i "$S:s=44100:d=0.5" -c:a wmav2 -b:a 64k -map_metadata -1 -fflags +bitexact "$T/a.wma"
ffmpeg -v error -f lavfi -i "$S:s=44100:d=0.5" -c:a libvorbis -q:a 2 -map_metadata -1 -fflags +bitexact "$T/a.ogg"
{
  echo "/* mfmore_gen.sh */"
  for n in flac wma ogg; do
    echo "static const unsigned char file_$n[] = {"
    od -An -v -tx1 "$T/a.$n" | sed 's/ \([0-9a-f][0-9a-f]\)/0x\1,/g'
    echo "};"
  done
} > mfmore_data.h
rm -r "$T"
