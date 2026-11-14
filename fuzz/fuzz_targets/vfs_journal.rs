//! kura's shared namespace (vfs/shared.rs, review P1-3): any bytes as a journal entry (refused
//! or applied whole), then as three processes' batches of puts, writes, size changes, opens,
//! closes, unlinks and mkdirs through one buffer (sometimes too small): after each publish every
//! process has exactly the namespace of the one that published, handles valid, free slots the
//! empty ones (kura::vfs::fuzz_journal, vfs/shared/sim.rs).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| kura::vfs::fuzz_journal(data));
