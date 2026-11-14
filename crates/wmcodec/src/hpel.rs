//! Motion compensation pixel operations (FFmpeg's hpeldsp put / put_no_rnd and wmv2dsp's
//! mspel filters). Sources are blocks with the pixels around them (see `window`).

/// A `w` x `h` block of `plane` (stride `stride`, `pw` x `ph` pixels) at (x, y), pixels
/// outside repeated from the edge (emulated_edge_mc).
pub fn window(plane: &[u8], stride: usize, pw: i32, ph: i32, x: i32, y: i32, w: usize, h: usize, out: &mut [u8]) {
    for j in 0..h {
        let sy = (y + j as i32).clamp(0, ph - 1) as usize;
        let row = &plane[sy * stride..];
        for i in 0..w {
            let sx = (x + i as i32).clamp(0, pw - 1) as usize;
            out[j * w + i] = row[sx];
        }
    }
}

/// put_pixels / put_no_rnd_pixels, `w` x `h`, half-pel position `dxy` (bit 0: x, bit 1: y);
/// `src` holds (w + 1) x (h + 1) pixels with stride `ss`.
pub fn hpel(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], ss: usize, w: usize, h: usize, dxy: usize, rnd: bool) {
    let r = rnd as u32;
    for y in 0..h {
        for x in 0..w {
            let p = |dx: usize, dy: usize| src[(y + dy) * ss + x + dx] as u32;
            let v = match dxy & 3 {
                0 => p(0, 0),
                1 => (p(0, 0) + p(1, 0) + r) >> 1,
                2 => (p(0, 0) + p(0, 1) + r) >> 1,
                _ => (p(0, 0) + p(1, 0) + p(0, 1) + p(1, 1) + 1 + r) >> 2,
            };
            dst[doff + y * ds + x] = v as u8;
        }
    }
}

#[inline]
fn tap(a: u8, b: u8, c: u8, d: u8) -> u8 {
    ((9 * (b as i32 + c as i32) - (a as i32 + d as i32) + 8) >> 4).clamp(0, 255) as u8
}

/// wmv2_mspel8_h_lowpass: `h` rows of 8 from `src` at `so` (it reads one left, two right).
fn h_low(dst: &mut [u8], ds: usize, src: &[u8], so: usize, ss: usize, h: usize) {
    for y in 0..h {
        let s = so + y * ss;
        for x in 0..8 {
            dst[y * ds + x] = tap(src[s + x - 1], src[s + x], src[s + x + 1], src[s + x + 2]);
        }
    }
}

/// wmv2_mspel8_v_lowpass: 8 x 8 (it reads one row up, two down).
fn v_low(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], so: usize, ss: usize) {
    for x in 0..8 {
        for y in 0..8 {
            let s = |k: isize| src[(so as isize + x as isize + (y as isize + k) * ss as isize) as usize];
            dst[doff + y * ds + x] = tap(s(-1), s(0), s(1), s(2));
        }
    }
}

fn avg(dst: &mut [u8], doff: usize, ds: usize, a: &[u8], ao: usize, as_: usize, b: &[u8], bs: usize) {
    for y in 0..8 {
        for x in 0..8 {
            dst[doff + y * ds + x] = ((a[ao + y * as_ + x] as u32 + b[y * bs + x] as u32 + 1) >> 1) as u8;
        }
    }
}

/// put_mspel_pixels_tab[dxy] for an 8x8 block: `src` at `so` with at least one pixel / row
/// before it and two after (stride `ss`).
pub fn mspel8(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], so: usize, ss: usize, dxy: usize) {
    let mut half = [0u8; 64];
    let mut hh = [0u8; 88];
    let mut hv = [0u8; 64];
    match dxy {
        0 => {
            for y in 0..8 {
                dst[doff + y * ds..doff + y * ds + 8].copy_from_slice(&src[so + y * ss..so + y * ss + 8]);
            }
        }
        1 | 3 => {
            h_low(&mut half, 8, src, so, ss, 8);
            avg(dst, doff, ds, src, so + (dxy == 3) as usize, ss, &half, 8);
        }
        2 => h_low(&mut dst[doff..], ds, src, so, ss, 8),
        4 => v_low(dst, doff, ds, src, so, ss),
        5 | 7 => {
            h_low(&mut hh, 8, src, so - ss, ss, 11);
            v_low(&mut half, 0, 8, src, so + (dxy == 7) as usize, ss);
            v_low(&mut hv, 0, 8, &hh, 8, 8);
            avg(dst, doff, ds, &half, 0, 8, &hv, 8);
        }
        _ => {
            h_low(&mut hh, 8, src, so - ss, ss, 11);
            v_low(dst, doff, ds, &hh, 8, 8);
        }
    }
}
