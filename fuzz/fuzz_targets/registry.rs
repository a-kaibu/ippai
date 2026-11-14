//! sake's registry file (misc.rs parse_reg / format_reg) and the merge of two processes'
//! changes (merge_reg) on any bytes: no panic, what is written reads back, keys not changed
//! here become the file's, nothing changed here gives the file, an unchanged file keeps ours,
//! merging again changes nothing (sake::misc::reg_fuzz::run).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    sake::misc::reg_fuzz::run(data);
});
