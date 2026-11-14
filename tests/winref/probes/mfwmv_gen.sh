#!/bin/bash
# Writes mfwmv_data.h: the ASF movie the mfwmv probe plays with Media Foundation, made with FFmpeg
# (the same as crates/sake/src/mf/testdata/colors.wmv): WMV2 64x48 at 24 fps, 18 frames (6 red,
# 6 lime, 6 blue), a key frame every 6, WMA2 stereo 440 Hz for 0.75 s. Run again only to change it
# (the bytes are what Windows' answers were recorded from).
set -euo pipefail
cd "$(dirname "$0")"
{
  echo "/* mfwmv_gen.sh */"
  echo "static const unsigned char file_wmv[] = {"
  od -An -v -tx1 ../../../crates/sake/src/mf/testdata/colors.wmv | sed 's/ \([0-9a-f][0-9a-f]\)/0x\1,/g'
  echo "};"
} > mfwmv_data.h
