//! d3d11.dll and dxgi.dll: kura's Direct3D 11 / DXGI (crates/kura/src/d3d11), the module
//! Wine's dlls/vnd3d11 and dlls/vndxgi hand their calls to. As in d3d9.rs, every vtable entry
//! is a trap whose handler passes the guest's stack arguments to the host as
//! `interface << 8 | index`.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::api::{host_fn, implement, Ctx};
use crate::mem;
use kura::d3d11::{CREATE_DEVICE, CREATE_DEVICE10, CREATE_FACTORY};

#[path = "d3d11_methods.rs"]
mod methods;
use methods::METHODS;

const FIRST: u32 = 64;
const E_NOTIMPL: u32 = 0x8000_4001;

/// trap address -> method id (0 marks a FLOAT method)
static IDS: Mutex<Option<HashMap<u32, (u32, bool)>>> = Mutex::new(None);

fn calls() -> &'static kura::d3d9::HostCalls {
    crate::d3d9::host_calls()
}

fn method(c: &mut Ctx) -> u32 {
    let (id, float) = IDS.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap).copied()).unwrap_or((0, false));
    if float {
        c.st0 = Some(0.0);
        return 0;
    }
    if mem::x64() {
        // (the host reads 8-byte slots)
        let a: [u64; 16] = std::array::from_fn(|i| c.a64(i as u32));
        return unsafe { kura::d3d11::call(id, a.as_ptr() as *const u32, calls()) };
    }
    unsafe { kura::d3d11::call(id, (c.esp + 4) as usize as *const u32, calls()) }
}

fn host(id: u32, args: &[u32]) -> u32 {
    init();
    if mem::x64() {
        let mut a = [0u64; 16];
        for (d, &s) in a.iter_mut().zip(args) {
            *d = s as u64;
        }
        return unsafe { kura::d3d11::call(id, a.as_ptr() as *const u32, calls()) };
    }
    unsafe { kura::d3d11::call(id, args.as_ptr(), calls()) }
}

/// The vtables, given to the host once (Wine's d3d11.dll DllMain).
fn init() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let n = FIRST + METHODS.len() as u32;
        let ps = mem::ps();
        let table = mem::heap_alloc(ps * n, true);
        let mut ids = HashMap::default();
        for (k, m) in METHODS.iter().enumerate() {
            let i = FIRST + k as u32;
            let v = mem::heap_alloc(ps * m.len() as u32, true);
            for (j, &args) in m.iter().enumerate() {
                let float = args == 0;
                let bytes = if float { 8 } else { 4 * args as u32 };
                let a = host_fn(&format!("d3d11:{i}:{j}"), method, bytes);
                ids.insert(a, (i << 8 | j as u32, float));
                mem::wp(v + ps * j as u32, a);
            }
            mem::wp(table + ps * i, v);
        }
        *IDS.lock().unwrap() = Some(ids);
        unsafe { kura::d3d11::init(table as usize as *const u32, n) };
    });
}

/// (adapter, driver type, software, flags, levels, count, sdk, swapchain desc, swapchain,
/// device, feature level, context)
fn D3D11CreateDeviceAndSwapChain(c: &mut Ctx) -> u32 {
    let a: Vec<u32> = (0..12).map(|i| c.a(i)).collect();
    host(CREATE_DEVICE, &a)
}
fn D3D11CreateDevice(c: &mut Ctx) -> u32 {
    let a = [c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), 0, 0, c.a(7), c.a(8), c.a(9)];
    host(CREATE_DEVICE, &a)
}
/// (factory, adapter, flags, levels, count, device)
fn D3D11CoreCreateDevice(c: &mut Ctx) -> u32 {
    let a = [c.a(1), 0, 0, c.a(2), c.a(3), c.a(4), 7, 0, 0, c.a(5), 0, 0];
    host(CREATE_DEVICE, &a)
}
/// d3d10.dll / d3d10_1.dll: the D3D11 device's D3D10 face (crates/kura/src/d3d11/d3d10.rs)
/// (adapter, driver type, software, flags, sdk, out)
fn D3D10CreateDevice(c: &mut Ctx) -> u32 {
    host(CREATE_DEVICE10, &[c.a(0), c.a(1), c.a(2), c.a(3), 0xa000, c.a(4), 0, 0, c.a(5)])
}
/// (adapter, driver type, software, flags, sdk, swap chain desc, swap chain, device)
fn D3D10CreateDeviceAndSwapChain(c: &mut Ctx) -> u32 {
    host(CREATE_DEVICE10, &[c.a(0), c.a(1), c.a(2), c.a(3), 0xa000, c.a(4), c.a(5), c.a(6), c.a(7)])
}
/// (adapter, driver type, software, flags, level, sdk, out)
fn D3D10CreateDevice1(c: &mut Ctx) -> u32 {
    host(CREATE_DEVICE10, &[c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), 0, 0, c.a(6)])
}
/// (adapter, driver type, software, flags, level, sdk, swap chain desc, swap chain, device)
fn D3D10CreateDeviceAndSwapChain1(c: &mut Ctx) -> u32 {
    let a: Vec<u32> = (0..9).map(|i| c.a(i)).collect();
    host(CREATE_DEVICE10, &a)
}
fn D3D11On12CreateDevice(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}
fn CreateDXGIFactory(c: &mut Ctx) -> u32 {
    host(CREATE_FACTORY, &[0, c.a(0), c.a(1)])
}
fn CreateDXGIFactory2(c: &mut Ctx) -> u32 {
    host(CREATE_FACTORY, &[c.a(0), c.a(1), c.a(2)])
}
fn DXGIGetDebugInterface1(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), 0);
    E_NOTIMPL
}
fn not_impl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}
fn zero(_: &mut Ctx) -> u32 {
    0
}

pub fn register() {
    implement("d3d11", "D3D11CreateDeviceAndSwapChain", D3D11CreateDeviceAndSwapChain);
    implement("d3d11", "D3D11CreateDevice", D3D11CreateDevice);
    implement("d3d11", "D3D11CoreCreateDevice", D3D11CoreCreateDevice);
    implement("d3d11", "D3D11On12CreateDevice", D3D11On12CreateDevice);
    for n in ["D3D11CoreCreateLayeredDevice", "D3D11CoreGetLayeredDeviceSize", "D3D11CoreRegisterLayers"] {
        implement("d3d11", n, not_impl);
    }
    implement("d3d10", "D3D10CreateDevice", D3D10CreateDevice);
    implement("d3d10", "D3D10CreateDeviceAndSwapChain", D3D10CreateDeviceAndSwapChain);
    implement("d3d10_1", "D3D10CreateDevice1", D3D10CreateDevice1);
    implement("d3d10_1", "D3D10CreateDeviceAndSwapChain1", D3D10CreateDeviceAndSwapChain1);
    implement("dxgi", "CreateDXGIFactory", CreateDXGIFactory);
    implement("dxgi", "CreateDXGIFactory1", CreateDXGIFactory);
    implement("dxgi", "CreateDXGIFactory2", CreateDXGIFactory2);
    implement("dxgi", "DXGIGetDebugInterface1", DXGIGetDebugInterface1);
    implement("dxgi", "DXGID3D10CreateDevice", not_impl);
    implement("dxgi", "DXGID3D10RegisterLayers", not_impl);
    implement("dxgi", "DXGIDeclareAdapterRemovalSupport", zero);
}
