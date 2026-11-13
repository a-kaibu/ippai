//! Lists an ASF file's streams and samples (stream, time in ms, size, key): `asfdump FILE`.
fn main() {
    let path = std::env::args().nth(1).expect("asfdump FILE");
    let mut r = asf::Reader::new(std::fs::read(path).unwrap()).unwrap();
    for s in &r.header.streams {
        println!("stream {} {:?} codec {:#x} spread {:?} format {} bytes", s.number, s.kind, s.codec(), s.spread, s.format.len());
    }
    println!("duration {} preroll {} packets {} x {}", r.header.duration, r.header.preroll, r.header.packets, r.header.packet_size);
    for a in &r.header.attributes {
        println!("attr {} {} {:?}", a.stream, a.name, a.value);
    }
    while let Some(s) = r.next_sample() {
        println!("{},{},{},{}", s.stream, s.time / 10_000, s.data.len(), if s.key { "K" } else { "_" });
    }
}
