//! The browser transport for rustls: only ciphertext crosses these imports.
use std::{
    io::{self, Read, Write},
    time::{Duration, Instant},
};
#[cfg(target_os = "emscripten")]
unsafe extern "C" {
    fn sake_tls_open(target: u32, timeout: u32) -> i32;
    fn sake_tls_write(id: i32, p: *const u8, n: usize, timeout: u32) -> i32;
    fn sake_tls_read(id: i32, p: *mut u8, n: usize, timeout: u32) -> i32;
    fn sake_tls_close(id: i32);
}
pub(crate) struct Socket {
    pub id: i32,
    /// each read's and write's own limit (WinINet's timeout: the time one wait may take)
    timeout: u32,
    opened: Instant,
    /// the last read's bytes (one buffer, kept), and how far they were handed out
    pending: Vec<u8>,
    at: usize,
}

/// A read big enough for the relay to fill directly (rustls asks for a record and more)
const DIRECT: usize = 16 * 1024;
const CHUNK: usize = 65536;

/// One Read::read: what is left of `pending` first; else `out` filled by `refill` straight
/// away when it is big, or `pending` refilled (no zeroing: the buffer is kept) and served.
/// `refill` returns how many bytes it put in the slice (0: the end).
fn serve(pending: &mut Vec<u8>, at: &mut usize, out: &mut [u8], mut refill: impl FnMut(&mut [u8]) -> io::Result<usize>) -> io::Result<usize> {
    if out.is_empty() {
        return Ok(0);
    }
    if *at >= pending.len() {
        if out.len() >= DIRECT {
            let n = refill(out)?;
            if n > out.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "TLS relay read size"));
            }
            return Ok(n);
        }
        pending.resize(CHUNK, 0);
        let n = refill(&mut pending[..])?;
        if n > CHUNK {
            pending.clear();
            return Err(io::Error::new(io::ErrorKind::InvalidData, "TLS relay read size"));
        }
        pending.truncate(n);
        *at = 0;
        if n == 0 {
            return Ok(0);
        }
    }
    let n = out.len().min(pending.len() - *at);
    out[..n].copy_from_slice(&pending[*at..*at + n]);
    *at += n;
    Ok(n)
}
/// The whole exchange at most, however much keeps flowing: a peer that trickles a byte now and
/// then stops the guest thread no longer than this.
const TOTAL: Duration = Duration::from_secs(600);
/// The limit of the next read or write (ms): the socket's timeout, or what is left of TOTAL
/// since `opened` if less; None once TOTAL has passed. (A limit fixed at the open, as before,
/// cut every download longer than one timeout in the middle.)
fn op_limit(timeout: u32, opened: Instant, now: Instant) -> Option<u32> {
    let left = TOTAL.saturating_sub(now.saturating_duration_since(opened)).as_millis();
    (left > 0).then(|| left.min(timeout as u128) as u32)
}
impl Socket {
    pub fn open(host: &str, timeout: u32) -> io::Result<Self> {
        let Some(target) = ippai_tls::target(host) else {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "TLS destination unavailable"));
        };
        let timeout = timeout.clamp(100, 120000);
        #[cfg(target_os = "emscripten")]
        let id = unsafe { sake_tls_open(target, timeout) };
        #[cfg(not(target_os = "emscripten"))]
        let id = {
            let _ = (target, timeout);
            -1
        };
        if id < 0 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "TLS relay unavailable",
            ));
        }
        Ok(Self {
            id,
            timeout,
            opened: Instant::now(),
            pending: Vec::new(),
            at: 0,
        })
    }
    fn remaining(&self) -> io::Result<u32> {
        op_limit(self.timeout, self.opened, Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "TLS relay deadline"))
    }
}
impl Read for Socket {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let (id, limit) = (self.id, self.remaining());
        serve(&mut self.pending, &mut self.at, out, |buf| {
            let timeout = limit.as_ref().map_err(|e| io::Error::new(e.kind(), e.to_string()))?;
            #[cfg(target_os = "emscripten")]
            let n = unsafe { sake_tls_read(id, buf.as_mut_ptr(), buf.len(), *timeout) };
            #[cfg(not(target_os = "emscripten"))]
            let n = {
                let _ = (id, timeout, &buf);
                -1
            };
            if n < 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "TLS relay read failed"));
            }
            Ok(n as usize)
        })
    }
}
impl Write for Socket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let n = bytes.len().min(65536);
        let timeout = self.remaining()?;
        #[cfg(target_os = "emscripten")]
        let written = unsafe { sake_tls_write(self.id, bytes.as_ptr(), n, timeout) };
        #[cfg(not(target_os = "emscripten"))]
        let written = {
            let _ = (n, timeout);
            -1
        };
        if written <= 0 || written as usize > n {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "TLS relay write failed",
            ))
        } else {
            Ok(written as usize)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        #[cfg(target_os = "emscripten")]
        unsafe {
            sake_tls_close(self.id)
        }
        #[cfg(not(target_os = "emscripten"))]
        let _ = self.id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// review b09: any stream the relay hands over in any chunks reads back whole and in
        /// order through any sizes of read (small ones from the kept buffer, big ones straight)
        #[test]
        fn reads_give_the_stream(chunks in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 1..3000), 0..12), sizes in proptest::collection::vec(prop_oneof![1usize..64, 1usize..4000, DIRECT..DIRECT * 3], 1..64)) {
            let want: Vec<u8> = chunks.concat();
            let mut src = chunks.into_iter();
            let mut carry: Vec<u8> = Vec::new();
            let mut refill = |buf: &mut [u8]| -> io::Result<usize> {
                if carry.is_empty() {
                    carry = src.next().unwrap_or_default();
                }
                let n = buf.len().min(carry.len());
                buf[..n].copy_from_slice(&carry[..n]);
                carry.drain(..n);
                Ok(n)
            };
            let (mut pending, mut at, mut got) = (Vec::new(), 0, Vec::new());
            let mut i = 0;
            loop {
                let mut out = vec![0u8; sizes[i % sizes.len()]];
                i += 1;
                let n = serve(&mut pending, &mut at, &mut out, &mut refill).unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&out[..n]);
            }
            prop_assert_eq!(got, want);
            prop_assert!(pending.capacity() <= CHUNK);
        }
    }

    /// A relay that claims more than it was given is an error, not a panic
    #[test]
    fn oversize_relay_answers_are_errors() {
        let (mut p, mut at) = (Vec::new(), 0);
        let mut out = [0u8; 10];
        assert!(serve(&mut p, &mut at, &mut out, |b| Ok(b.len() + 1)).is_err());
        let mut big = vec![0u8; DIRECT];
        assert!(serve(&mut p, &mut at, &mut big, |b| Ok(b.len() + 1)).is_err());
    }

    /// Each read or write gets the whole timeout again, however long the exchange has run, up
    /// to TOTAL; then none.
    #[test]
    fn every_operation_gets_its_own_timeout_within_the_total() {
        let t0 = Instant::now();
        assert_eq!(op_limit(30_000, t0, t0), Some(30_000));
        assert_eq!(op_limit(30_000, t0, t0 + Duration::from_secs(200)), Some(30_000));
        assert_eq!(op_limit(30_000, t0, t0 + TOTAL - Duration::from_secs(10)), Some(10_000));
        assert_eq!(op_limit(30_000, t0, t0 + TOTAL), None);
        assert_eq!(op_limit(30_000, t0, t0 + TOTAL + Duration::from_secs(1)), None);
        // (a clock read before the open: the full timeout, not an underflow)
        assert_eq!(op_limit(100, t0 + Duration::from_secs(1), t0), Some(100));
    }
}
