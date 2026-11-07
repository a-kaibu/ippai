#!/bin/bash
# Writes mfreader_data.h: the sound files the mfreader probe opens with the Source Reader, made
# with FFmpeg: an M4A (AAC-LC 44.1 kHz stereo, 440 Hz left, 660 Hz right), an MP3 of the same
# (libmp3lame, with an ID3v2 tag), a 16-bit WAVE (22.05 kHz mono 550 Hz). Run again only to
# change the set (the bytes are what Windows' answers were recorded from).
set -euo pipefail
cd "$(dirname "$0")"
T=$(mktemp -d)
S="aevalsrc=0.5*sin(2*PI*440*t)|0.25*sin(2*PI*660*t):s=44100:d=0.5"
ffmpeg -v error -f lavfi -i "$S" -c:a aac -b:a 96k -map_metadata -1 -fflags +bitexact -flags:a +bitexact "$T/a.m4a"
ffmpeg -v error -f lavfi -i "$S" -c:a libmp3lame -b:a 96k -write_xing 0 -id3v2_version 3 -metadata title=probe "$T/a.mp3"
ffmpeg -v error -f lavfi -i "aevalsrc=0.5*sin(2*PI*550*t):s=22050:d=0.3" -c:a pcm_s16le -map_metadata -1 -fflags +bitexact "$T/a.wav"
{
  echo "/* mfreader_gen.sh */"
  for n in m4a mp3 wav; do
    echo "static const unsigned char file_$n[] = {"
    od -An -v -tx1 "$T/a.$n" | sed 's/ \([0-9a-f][0-9a-f]\)/0x\1,/g'
    echo "};"
  done
} > mfreader_data.h
rm -r "$T"
