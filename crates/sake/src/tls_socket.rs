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
    deadline: Instant,
    pending: VecDeque<u8>,
}
impl Socket {
    pub fn open(host: &str, timeout: u32) -> io::Result<Self> {
        let target = match host {
            "charge1.soft-denchi.jp" => 0,
            "dlsoft.dmm.co.jp" => 1,
            "softdenchi-doujin-api.dmm.co.jp" => 2,
            _ => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "TLS destination unavailable")),
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
            deadline: Instant::now() + Duration::from_millis(timeout as u64),
            pending: VecDeque::new(),
        })
    }
    fn remaining(&self) -> io::Result<u32> {
        let ms = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        if ms == 0 {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS relay deadline",
            ))
        } else {
            Ok(ms.min(u32::MAX as u128) as u32)
        }
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
    }
}
