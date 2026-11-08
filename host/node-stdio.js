// Node: a write to stdout or stderr that finds the pipe full waits for it. Node may give the
// process non-blocking pipes, and a write then fails with EAGAIN, which Rust's eprintln! turns
// into a panic ("failed printing to stderr: Resource temporarily unavailable"): a program that
// writes a lot while the reader is slow ended there (shlwapi:url's 13000 failure lines, with
// other tests running beside it).
(function () {
  if (typeof process !== 'object' || !process.versions || !process.versions.node) return;
  const fs = require('fs');
  if (fs.writeSync.vnStdio) return;
  const write = fs.writeSync;
  const nap = new Int32Array(new SharedArrayBuffer(4));
  const patched = function (fd, buf, ...rest) {
    if (fd !== 1 && fd !== 2) return write.call(fs, fd, buf, ...rest);
    for (let wait = 1; ; wait = Math.min(wait * 2, 50)) {
      try {
        return write.call(fs, fd, buf, ...rest);
      } catch (e) {
        if (e.code !== 'EAGAIN') throw e;
        Atomics.wait(nap, 0, 0, wait);
      }
    }
  };
  patched.vnStdio = true;
  fs.writeSync = patched;
})();
