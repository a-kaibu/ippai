//! Media Foundation without playback: MFStartup fails, so games that play their movies with
//! it (Artemis) skip them, as on a Windows N edition without the Media Feature Pack.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::mem::putp;

const E_FAIL: u32 = 0x8000_4005;
const E_NOTIMPL: u32 = 0x8000_4001;

fn MFStartup(_: &mut Ctx) -> u32 {
    E_FAIL
}
fn MFShutdown(_: &mut Ctx) -> u32 {
    0
}
/// out pointer in argument `i`: cleared
fn none(c: &mut Ctx, i: u32) -> u32 {
    let p = c.a(i);
    if p != 0 {
        putp(p, 0);
    }
    E_NOTIMPL
}

pub fn init() {
    implement("mfplat", "MFStartup", MFStartup);
    implement("mfplat", "MFShutdown", MFShutdown);
    implement("mfplat", "MFCreateSourceResolver", |c| none(c, 0));
    implement("mf", "MFCreateMediaSession", |c| none(c, 1));
    implement("mf", "MFCreateTopology", |c| none(c, 0));
    implement("mf", "MFCreateTopologyNode", |c| none(c, 1));
    implement("mf", "MFGetService", |c| none(c, 3));
    implement("mf", "MFCreateVideoRendererActivate", |c| none(c, 1));
    implement("mf", "MFCreateAudioRendererActivate", |c| none(c, 0));
}
