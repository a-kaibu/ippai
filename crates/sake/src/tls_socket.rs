//! The browser transport for rustls: only ciphertext crosses these imports.
use std::{
    collections::VecDeque,
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
    pending: VecDeque<u8>,
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
            pending: VecDeque::new(),
        })
    }
    fn remaining(&self) -> io::Result<u32> {
        op_limit(self.timeout, self.opened, Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "TLS relay deadline"))
    }
}
impl Read for Socket {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.pending.is_empty() {
            let mut bytes = vec![0u8; 65536];
            let timeout = self.remaining()?;
            #[cfg(target_os = "emscripten")]
            let n = unsafe { sake_tls_read(self.id, bytes.as_mut_ptr(), bytes.len(), timeout) };
            #[cfg(not(target_os = "emscripten"))]
            let n = {
                let _ = timeout;
                -1
            };
            if n < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "TLS relay read failed",
                ));
            }
            if n == 0 {
                return Ok(0);
            }
            if n as usize > bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "TLS relay read size",
                ));
            }
            bytes.truncate(n as usize);
            self.pending.extend(bytes);
        }
        let n = out.len().min(self.pending.len());
        for b in &mut out[..n] {
            *b = self.pending.pop_front().unwrap()
        }
        Ok(n)
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
