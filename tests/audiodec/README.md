# AudioDecoder (WebCodecs) vs crates/vorbis in the browser

Decodes the same streams in Chromium with crates/vorbis built for wasm (as kura would decode a
movie's sound) and with WebCodecs' AudioDecoder, and prints, per file: ms of decoding per
second of sound (wasm; AudioDecoder's wall time, its callbacks on the page thread, and the CPU
of all Chromium processes, since its decoder runs off the page thread), how long one packet
waits for its samples when each is waited for, and how far apart the two decodes are. It also
asks AudioDecoder which codecs it takes.

    cargo build --release --target wasm32-unknown-unknown \
      --manifest-path tests/audiodec/vorbis-wasm/Cargo.toml
    node tests/audiodec/run.mjs \
      tests/audiodec/vorbis-wasm/target/wasm32-unknown-unknown/release/vorbis_wasm.wasm \
      crates/vorbis/testdata/stereo-q5.ogg some.ogg some.opus some.mp3

`ROUNDS` (default 5): decodes per file (the median is printed). `PLAYWRIGHT`: where playwright
is, when not importable as `playwright`.

Note: a 5.1 stream's channels come out of AudioDecoder in another order than Vorbis's (ffmpeg
reorders them), so its difference is large; mono and stereo match within 2.4e-7.
