#!/usr/bin/env bash
# Downloads the WMV 9 / VC-1 Simple / Main / Complex files of FFmpeg's sample collections
# (samples.ffmpeg.org, fate-suite.ffmpeg.org) into DIR, for the differential test:
#   crates/wmcodec/samples.sh DIR && WMV_DIR=DIR cargo test -p wmcodec --release files_from_dir -- --nocapture
# (each file's pictures must be FFmpeg's, bit for bit; ffmpeg must be on PATH)
set -eu
dir=${1:?usage: samples.sh DIR}
mkdir -p "$dir"
s=https://samples.ffmpeg.org/V-codecs/WMV9
f=https://fate-suite.ffmpeg.org
for u in \
  $s/B-frames/bloodyback.wmv $s/B-frames/guitar.wmv $s/BI-frames/psychonauts.wmv \
  $s/RES_RTM-pframes/niceday.wmv $s/Video_smoothing/Video_Smoothing.wmv \
  $s/kelsy/kelsy_56K.wmv $s/kelsy/kelsy_300.wmv \
  $s/wmv-samples/sample-wmv9-simple-SP@ML.wmv $s/wmv-samples/sample-wmv9-main-MP@LL.wmv \
  $s/wmv-samples/sample-wmv9-complex-CP.wmv $s/trailers/pinball.wmv $s/trailers/snowboard.wmv \
  $s/buggyDMOdecoding.wmv $s/nokia_n90.wmv $s/b52crash2.wmv \
  $f/vc1/SMM0005.rcv $f/vc1/SMM0015.rcv $f/wmv8/wmv8_x8intra.wmv; do
  out=$dir/$(basename "$u")
  [ -s "$out" ] || curl -fsS --retry 3 -o "$out" "$u"
done
