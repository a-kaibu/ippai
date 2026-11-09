//! The internal UI's dialogs (the Dialog, Control, ControlEvent tables).
//!
//! For now a dialog runs as at /qb: it is not shown, and its "Next"/"Install" path is taken
//! (ERROR_SUCCESS, the sequence goes on).

use std::sync::{Arc, Mutex};

use crate::api::Ctx;
use crate::msi_pkg::Package;

pub fn dialog(_c: &mut Ctx, _p: &Arc<Mutex<Package>>, name: &str) -> u32 {
    if crate::env_on!("SAKE_TRACE_MSI") {
        eprintln!("msi: dialog {name} (not shown)");
    }
    msidb::ERROR_SUCCESS
}
