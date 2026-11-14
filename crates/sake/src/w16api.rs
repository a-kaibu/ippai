//! Win16 functions (KERNEL, USER, GDI...): most convert their arguments and call sake's 32-bit
//! handler (the `SIMPLE` table); the rest are here. Messages crossing between 16-bit window
//! procedures and the 32-bit window manager are converted by `to16` / `to32`.

use crate::mem::{cstr, r16, r32, w16, w32, w8};
use crate::w16::{self, call16, call32, h16, h32, lin, task, C16, A};
use crate::HashMap;
use std::sync::{Mutex, OnceLock};
use koji::{Cpu, EAX, ECX, EDX, ESP};

/// (module.name, 32-bit dll!function, argument conversions, result conversion).
/// Arguments, in the 16-bit order: w zero-extend, s sign-extend, l as is, h handle, p linear
/// pointer (also of a far pointer), r RECT16 in, R RECT16 out, P POINT16 out, i instance (0),
/// 0 inserts a zero. Result: i int16, h handle, l long.
const SIMPLE: &[(&str, &str, &str, &str)] = &[
    ("USER.MessageBox", "user32!MessageBoxA", "hppw", "i"),
    ("USER.KillTimer", "user32!KillTimer", "hw", "i"),
    ("USER.UpdateWindow", "user32!UpdateWindow", "h", "i"),
    ("USER.InvalidateRect", "user32!InvalidateRect", "hrw", "i"),
    ("USER.ValidateRect", "user32!ValidateRect", "hr", "i"),
    ("USER.SetFocus", "user32!SetFocus", "h", "h"),
    ("USER.GetFocus", "user32!GetFocus", "", "h"),
    ("USER.SelectPalette", "gdi32!SelectPalette", "hhw", "h"),
    ("USER.RealizePalette", "gdi32!RealizePalette", "h", "i"),
    ("USER.GetClientRect", "user32!GetClientRect", "hR", "i"),
    ("USER.GetWindowRect", "user32!GetWindowRect", "hR", "i"),
    ("USER.GetWindowText", "user32!GetWindowTextA", "hps", "i"),
    ("USER.GetWindowTextLength", "user32!GetWindowTextLengthA", "h", "i"),
    ("USER.SetWindowText", "user32!SetWindowTextA", "hp", "i"),
    ("USER.ShowWindow", "user32!ShowWindow", "hw", "i"),
    ("USER.MoveWindow", "user32!MoveWindow", "hsssss", "i"),
    ("USER.ScrollWindow", "user32!ScrollWindow", "hssrr", "i"),
    ("USER.SetScrollPos", "user32!SetScrollPos", "hwsw", "i"),
    ("USER.GetScrollPos", "user32!GetScrollPos", "hw", "i"),
    ("USER.SetScrollRange", "user32!SetScrollRange", "hwssw", "i"),
    ("USER.GetDC", "user32!GetDC", "h", "h"),
    ("USER.GetWindowDC", "user32!GetWindowDC", "h", "h"),
    ("USER.ReleaseDC", "user32!ReleaseDC", "hh", "i"),
    ("USER.EndDialog", "user32!EndDialog", "hs", "i"),
    ("USER.PostQuitMessage", "user32!PostQuitMessage", "s", "i"),
    ("USER.lstrcmp", "kernel32!lstrcmpA", "pp", "i"),
    ("USER.lstrcmpi", "kernel32!lstrcmpiA", "pp", "i"),
    ("USER.DestroyWindow", "user32!DestroyWindow", "h", "i"),
    ("USER.IsWindow", "user32!IsWindow", "h", "i"),
    ("USER.IsWindowVisible", "user32!IsWindowVisible", "h", "i"),
    ("USER.IsIconic", "user32!IsIconic", "h", "i"),
    ("USER.EnableWindow", "user32!EnableWindow", "hw", "i"),
    ("USER.GetParent", "user32!GetParent", "h", "h"),
    ("USER.SetCapture", "user32!SetCapture", "h", "h"),
    ("USER.ReleaseCapture", "user32!ReleaseCapture", "", "i"),
    ("USER.GetDlgItem", "user32!GetDlgItem", "hs", "h"),
    ("USER.SetDlgItemText", "user32!SetDlgItemTextA", "hsp", "i"),
    ("USER.GetDlgItemText", "user32!GetDlgItemTextA", "hspw", "i"),
    ("USER.SetDlgItemInt", "user32!SetDlgItemInt", "hsww", "i"),
    ("USER.CheckDlgButton", "user32!CheckDlgButton", "hsw", "i"),
    ("USER.IsDlgButtonChecked", "user32!IsDlgButtonChecked", "hs", "i"),
    ("USER.GetSystemMetrics", "user32!GetSystemMetrics", "s", "i"),
    ("USER.GetSysColor", "user32!GetSysColor", "s", "l"),
    ("USER.MessageBeep", "user32!MessageBeep", "w", "i"),
    ("USER.SetCursor", "user32!SetCursor", "h", "h"),
    ("USER.ShowCursor", "user32!ShowCursor", "w", "i"),
    ("USER.GetActiveWindow", "user32!GetActiveWindow", "", "h"),
    ("USER.SetActiveWindow", "user32!SetActiveWindow", "h", "h"),
    ("USER.BringWindowToTop", "user32!BringWindowToTop", "h", "i"),
    ("USER.GetMenu", "user32!GetMenu", "h", "h"),
    ("USER.SetMenu", "user32!SetMenu", "hh", "i"),
    ("USER.DrawMenuBar", "user32!DrawMenuBar", "h", "i"),
    ("USER.GetSubMenu", "user32!GetSubMenu", "hs", "h"),
    ("USER.EnableMenuItem", "user32!EnableMenuItem", "hww", "i"),
    ("USER.CheckMenuItem", "user32!CheckMenuItem", "hww", "i"),
    ("USER.GetKeyState", "user32!GetKeyState", "s", "i"),
    ("USER.GetAsyncKeyState", "user32!GetAsyncKeyState", "s", "i"),
    ("USER.GetTickCount", "kernel32!GetTickCount", "", "l"),
    ("USER.GetCurrentTime", "kernel32!GetTickCount", "", "l"),
    ("USER.FillRect", "user32!FillRect", "hrh", "i"),
    ("USER.FrameRect", "user32!FrameRect", "hrh", "i"),
    ("USER.InvertRect", "user32!InvertRect", "hr", "i"),
    ("USER.DrawIcon", "user32!DrawIcon", "hssh", "i"),
    ("USER.WinHelp", "user32!WinHelpA", "hpwl", "i"),
    ("USER.SetWindowPos", "user32!SetWindowPos", "hhsssss", "i"),
    ("USER.InvalidateRgn", "user32!InvalidateRgn", "hhw", "i"),
    ("USER.ClientToScreen", "user32!ClientToScreen", "hP", "i"),
    ("USER.ScreenToClient", "user32!ScreenToClient", "hP", "i"),
    ("USER.GetCursorPos", "user32!GetCursorPos", "P", "i"),
    ("USER.SetCursorPos", "user32!SetCursorPos", "ss", "i"),
    ("USER.ShowScrollBar", "user32!ShowScrollBar", "hww", "i"),
    ("USER.IsWindowEnabled", "user32!IsWindowEnabled", "h", "i"),
    ("USER.CloseClipboard", "user32!CloseClipboard", "", "i"),
    ("GDI.TextOut", "gdi32!TextOutA", "hsspw", "i"),
    ("GDI.SelectObject", "gdi32!SelectObject", "hh", "h"),
    ("GDI.SetStretchBltMode", "gdi32!SetStretchBltMode", "hw", "i"),
    ("GDI.GetStockObject", "gdi32!GetStockObject", "w", "h"),
    ("GDI.SetTextColor", "gdi32!SetTextColor", "hl", "l"),
    ("GDI.SetBkColor", "gdi32!SetBkColor", "hl", "l"),
    ("GDI.SetBkMode", "gdi32!SetBkMode", "hw", "i"),
    ("GDI.SetTextAlign", "gdi32!SetTextAlign", "hw", "i"),
    ("GDI.SetROP2", "gdi32!SetROP2", "hw", "i"),
    ("GDI.CreatePalette", "gdi32!CreatePalette", "p", "h"),
    ("GDI.StretchDIBits", "gdi32!StretchDIBits", "hssssssssppwl", "i"),
    ("GDI.SetDIBitsToDevice", "gdi32!SetDIBitsToDevice", "hsssssswwppw", "i"),
    ("GDI.CreateSolidBrush", "gdi32!CreateSolidBrush", "l", "h"),
    ("GDI.CreatePatternBrush", "gdi32!CreatePatternBrush", "h", "h"),
    ("GDI.CreatePen", "gdi32!CreatePen", "wsl", "h"),
    ("GDI.CreateCompatibleDC", "gdi32!CreateCompatibleDC", "h", "h"),
    ("GDI.CreateCompatibleBitmap", "gdi32!CreateCompatibleBitmap", "hss", "h"),
    ("GDI.DeleteDC", "gdi32!DeleteDC", "h", "i"),
    ("GDI.BitBlt", "gdi32!BitBlt", "hsssshssl", "i"),
    ("GDI.StretchBlt", "gdi32!StretchBlt", "hsssshssssl", "i"),
    ("GDI.PatBlt", "gdi32!PatBlt", "hssssl", "i"),
    ("GDI.Rectangle", "gdi32!Rectangle", "hssss", "i"),
    ("GDI.Ellipse", "gdi32!Ellipse", "hssss", "i"),
    ("GDI.LineTo", "gdi32!LineTo", "hss", "i"),
    ("GDI.SetPixel", "gdi32!SetPixel", "hssl", "l"),
    ("GDI.GetPixel", "gdi32!GetPixel", "hss", "l"),
    ("GDI.SaveDC", "gdi32!SaveDC", "h", "i"),
    ("GDI.RestoreDC", "gdi32!RestoreDC", "hs", "i"),
    ("GDI.GetDeviceCaps", "gdi32!GetDeviceCaps", "hs", "i"),
    ("GDI.RealizePalette", "gdi32!RealizePalette", "h", "i"),
    ("GDI.SelectPalette", "gdi32!SelectPalette", "hhw", "h"),
    ("GDI.SetDIBits", "gdi32!SetDIBits", "hhwwppw", "i"),
    ("GDI.GetDIBits", "gdi32!GetDIBits", "hhwwppw", "i"),
    ("GDI.CreateDIBitmap", "gdi32!CreateDIBitmap", "hplppw", "h"),
    ("GDI.CreateRectRgn", "gdi32!CreateRectRgn", "ssss", "h"),
    ("GDI.SelectClipRgn", "gdi32!SelectClipRgn", "hh", "i"),
    ("GDI.IntersectClipRect", "gdi32!IntersectClipRect", "hssss", "i"),
    ("GDI.SetMapMode", "gdi32!SetMapMode", "hw", "i"),
    ("GDI.SetPaletteEntries", "gdi32!SetPaletteEntries", "hwwp", "i"),
    ("GDI.GetPaletteEntries", "gdi32!GetPaletteEntries", "hwwp", "i"),
    ("GDI.AnimatePalette", "gdi32!AnimatePalette", "hwwp", "i"),
    ("GDI.GetNearestColor", "gdi32!GetNearestColor", "hl", "l"),
    ("GDI.CreateFontIndirect", "gdi32!CreateFontIndirectA", "F", "h"),
    ("KERNEL.lstrlen", "kernel32!lstrlenA", "p", "i"),
    ("KERNEL.IsDBCSLeadByte", "kernel32!IsDBCSLeadByte", "w", "i"),
    ("KEYBOARD.GetKBCodePage", "kernel32!GetACP", "", "i"),
    ("KEYBOARD.GetKeyboardType", "user32!GetKeyboardType", "s", "i"),
    ("KEYBOARD.MapVirtualKey", "user32!MapVirtualKeyA", "ww", "i"),
    ("KEYBOARD.VkKeyScan", "user32!VkKeyScanA", "w", "i"),
    ("KERNEL.GetTickCount", "kernel32!GetTickCount", "", "l"),
    ("KERNEL.GetCurrentTime", "kernel32!GetTickCount", "", "l"),
    ("KERNEL.GetPrivateProfileInt", "kernel32!GetPrivateProfileIntA", "ppsp", "i"),
    ("KERNEL.GetPrivateProfileString", "kernel32!GetPrivateProfileStringA", "pppppwp", "i"),
    ("KERNEL.WritePrivateProfileString", "kernel32!WritePrivateProfileStringA", "pppp", "i"),
    ("KERNEL.GetProfileInt", "kernel32!GetProfileIntA", "pps", "i"),
    ("KERNEL.GetProfileString", "kernel32!GetProfileStringA", "pppw", "i"),
    ("KERNEL.WriteProfileString", "kernel32!WriteProfileStringA", "ppp", "i"),
    ("KERNEL.OutputDebugString", "kernel32!OutputDebugStringA", "p", "i"),
    ("MMSYSTEM.mciSendString", "winmm!mciSendStringA", "ppwh", "l"),
    ("MMSYSTEM.sndPlaySound", "winmm!sndPlaySoundA", "pw", "i"),
    ("MMSYSTEM.timeGetTime", "winmm!timeGetTime", "", "l"),
    ("MMSYSTEM.waveOutGetNumDevs", "winmm!waveOutGetNumDevs", "", "i"),
    ("MMSYSTEM.waveOutGetErrorText", "winmm!waveOutGetErrorTextA", "wpw", "i"),
    ("MMSYSTEM.waveOutClose", "winmm!waveOutClose", "w", "i"),
    ("MMSYSTEM.waveOutPause", "winmm!waveOutPause", "w", "i"),
    ("MMSYSTEM.waveOutRestart", "winmm!waveOutRestart", "w", "i"),
    ("MMSYSTEM.waveOutReset", "winmm!waveOutReset", "w", "i"),
    ("MMSYSTEM.waveOutSetVolume", "winmm!waveOutSetVolume", "wl", "i"),
    ("MMSYSTEM.waveOutGetVolume", "winmm!waveOutGetVolume", "wp", "i"),
];

/// SIMPLE by module, then name: a lookup with the call's own strings (each call used to format
/// "MODULE.name" and compare it with every entry).
fn simple_entry(module: &str, name: &str) -> Option<&'static (&'static str, &'static str, &'static str, &'static str)> {
    static BY: OnceLock<HashMap<&'static str, HashMap<&'static str, usize>>> = OnceLock::new();
    let by = BY.get_or_init(|| {
        let mut m: HashMap<&'static str, HashMap<&'static str, usize>> = HashMap::default();
        for (i, e) in SIMPLE.iter().enumerate() {
            if let Some((md, n)) = e.0.split_once('.') {
                m.entry(md).or_default().entry(n).or_insert(i);
            }
        }
        m
    });
    by.get(module).and_then(|m| m.get(name)).map(|&i| &SIMPLE[i])
}

fn simple(c: &mut C16) -> u32 {
    let Some(&(_, func, conv, ret)) = simple_entry(&c.module, &c.name) else {
        crate::w16::fatal16(c.cpu, &format!("w16: {}.{} has no 32-bit counterpart", c.module, c.name));
    };
    let mut args = vec![];
    let mut outs: Vec<(u8, u32, u32)> = vec![];
    let mut k = 0;
    for t in conv.bytes() {
        if t == b'0' {
            args.push(0);
            continue;
        }
        let v = c.args[k];
        // ('p' types are already linear; far pointers of 'g' types become so here)
        let raw_ptr = |v: u32| lin(v);
        args.push(match t {
            b'w' => v & 0xffff,
            b's' => v as u16 as i16 as i32 as u32,
            b'h' => h32(v as u16),
            b'p' => {
                if c.types[k] == b'g' {
                    raw_ptr(v)
                } else {
                    v
                }
            }
            b'i' => 0,
            b'r' | b'R' | b'P' | b'F' => {
                let p = if c.types[k] == b'g' { raw_ptr(v) } else { v };
                if p == 0 {
                    0
                } else {
                    let t32 = crate::mem::heap_alloc(64, true);
                    match t {
                        b'r' => rect_in(p, t32),
                        b'F' => logfont_in(p, t32),
                        b'R' => {}
                        _ => {
                            w32(t32, r16(p) as i16 as i32 as u32);
                            w32(t32 + 4, r16(p + 2) as i16 as i32 as u32);
                        }
                    }
                    outs.push((t, p, t32));
                    t32
                }
            }
            _ => v,
        });
        k += 1;
    }
    let r = call32(c.cpu, func, &args);
    for (t, p16, p32) in outs {
        match t {
            b'R' => rect_out(p32, p16),
            b'P' => {
                w16(p16, r32(p32) as u16);
                w16(p16 + 2, r32(p32 + 4) as u16);
            }
            _ => {}
        }
        crate::mem::heap_free(p32);
    }
    if ret == "h" { h16(r) as u32 } else { r }
}

pub fn rect_in(p16: u32, p32: u32) {
    for i in 0..4 {
        w32(p32 + 4 * i, r16(p16 + 2 * i) as i16 as i32 as u32);
    }
}

pub fn rect_out(p32: u32, p16: u32) {
    for i in 0..4 {
        w16(p16 + 2 * i, r32(p32 + 4 * i) as u16);
    }
}

/// LOGFONT16 -> LOGFONTA
fn logfont_in(p16: u32, p32: u32) {
    for i in 0..5 {
        w32(p32 + 4 * i, r16(p16 + 2 * i) as i16 as i32 as u32);
    }
    crate::mem::copy(p32 + 20, p16 + 10, 8 + 32);
}

fn reg(name: &str, f: w16::H16) {
    let (m, n) = name.split_once('.').unwrap();
    w16::implement16(m, n, f);
}

pub fn init() {
    for e in SIMPLE {
        reg(e.0, simple);
    }
    // KERNEL
    reg("KERNEL.InitTask", init_task);
    reg("KERNEL.Catch", catch);
    reg("KERNEL.Throw", throw);
    reg("KERNEL.WaitEvent", |_| 0);
    reg("KERNEL.GetVersion", |_| 0x0500_0a03);
    reg("KERNEL.GetWinFlags", |_| 0x0413);
    reg("KERNEL.LockSegment", |c| c.a(0));
    reg("KERNEL.UnlockSegment", |c| c.a(0));
    reg("KERNEL.GlobalAlloc", |c| w16::global_alloc(c.a(1), c.w(0) & 0x40 != 0) as u32);
    reg("KERNEL.GlobalReAlloc", |c| w16::global_realloc(c.w(0), c.a(1), c.w(2) & 0x40 != 0) as u32);
    reg("KERNEL.GlobalFree", |c| {
        w16::global_free(c.w(0));
        0
    });
    reg("KERNEL.GlobalLock", |c| if w16::sel_base(c.w(0)).is_some() { (c.a(0) & 0xffff) << 16 } else { 0 });
    reg("KERNEL.GlobalUnlock", |_| 1);
    reg("KERNEL.GlobalSize", |c| w16::global_size(c.w(0)));
    reg("KERNEL.GlobalHandle", |c| (c.a(0) & 0xffff) << 16 | (c.a(0) & 0xffff));
    reg("KERNEL.GlobalFlags", |_| 0);
    reg("KERNEL.GlobalCompact", |_| 0x0100_0000);
    reg("KERNEL.GetFreeSpace", |_| 0x0100_0000);
    reg("KERNEL.LocalAlloc", |c| {
        let ds = c.ds();
        w16::local_alloc(ds, c.w(1), c.w(0) & 0x40 != 0) as u32
    });
    reg("KERNEL.LocalReAlloc", |c| {
        let ds = c.ds();
        w16::local_realloc(ds, c.w(0), c.w(1), c.w(2) & 0x40 != 0) as u32
    });
    reg("KERNEL.LocalFree", |c| {
        let ds = c.ds();
        w16::local_free(ds, c.w(0));
        0
    });
    reg("KERNEL.LocalSize", |c| {
        let ds = c.ds();
        w16::local_size(ds, c.w(0)) as u32
    });
    reg("KERNEL.LocalLock", |c| c.a(0));
    reg("KERNEL.LocalUnlock", |_| 0);
    reg("KERNEL.LocalInit", |_| 1);
    reg("KERNEL.LocalCompact", |_| 0x8000);
    reg("KERNEL.GetDOSEnvironment", |_| (task().env as u32) << 16);
    reg("KERNEL.MakeProcInstance", |c| c.a(0));
    reg("KERNEL.FreeProcInstance", |_| 0);
    reg("KERNEL.GetModuleFileName", |c| {
        let s = crate::text::s2a(&task().path);
        crate::mem::put_cstr(c.a(1), c.s(2).max(0) as u32, &s)
    });
    reg("KERNEL.GetModuleHandle", |c| {
        let p = c.a(0);
        if p >> 16 == 0 {
            return if p == 0 { task().hinst as u32 } else { p & 0xffff };
        }
        w16::module_handle(&crate::text::a2s(&cstr(lin(p)))) as u32
    });
    reg("KERNEL.GetModuleUsage", |_| 1);
    reg("KERNEL.GetInstanceData", |_| 0);
    reg("KERNEL.FatalAppExit", |c| {
        eprintln!("sake w16: FatalAppExit {}", crate::text::a2s(&cstr(c.a(1))));
        unsafe { libc::exit(1) }
    });
    reg("KERNEL.FatalExit", |c| {
        eprintln!("sake w16: FatalExit");
        let _ = c;
        unsafe { libc::exit(1) }
    });
    reg("KERNEL.lstrcpy", |c| {
        let d = lin(c.a(0));
        let s = cstr(c.a(1));
        crate::mem::write(d, &s);
        w8(d + s.len() as u32, 0);
        c.a(0)
    });
    reg("KERNEL.lstrcpyn", |c| {
        let d = lin(c.a(0));
        let n = c.w(2) as usize;
        if n > 0 {
            let s = cstr(c.a(1));
            let k = s.len().min(n - 1);
            crate::mem::write(d, &s[..k]);
            w8(d + k as u32, 0);
        }
        c.a(0)
    });
    reg("KERNEL.lstrcat", |c| {
        let d = lin(c.a(0));
        let l = cstr(d).len() as u32;
        let s = cstr(c.a(1));
        crate::mem::write(d + l, &s);
        w8(d + l + s.len() as u32, 0);
        c.a(0)
    });
    reg("KERNEL._lopen", |c| {
        let r = call32(c.cpu, "kernel32!_lopen", &[c.a(0), c.a(1) & 0xffff]);
        if r == u32::MAX {
            eprintln!("sake w16: _lopen {:?} failed", String::from_utf8_lossy(&cstr(c.a(0))));
            0xffff
        } else {
            r
        }
    });
    reg("KERNEL._lcreat", |c| {
        let r = call32(c.cpu, "kernel32!_lcreat", &[c.a(0), c.a(1) & 0xffff]);
        if r == u32::MAX { 0xffff } else { r }
    });
    reg("KERNEL._lclose", |c| {
        call32(c.cpu, "kernel32!CloseHandle", &[c.a(0) & 0xffff]);
        0
    });
    reg("KERNEL._lread", |c| file_read(c, c.a(0) & 0xffff, lin(c.a(1)), c.a(2) & 0xffff));
    reg("KERNEL._hread", |c| file_read(c, c.a(0) & 0xffff, lin(c.a(1)), c.a(2)));
    // (the buffer is a "p" in the spec, already linear; _lread's is a "g", a segmented pointer)
    reg("KERNEL._lwrite", |c| file_write(c, c.a(0) & 0xffff, c.a(1), c.a(2) & 0xffff));
    reg("KERNEL._hwrite", |c| file_write(c, c.a(0) & 0xffff, c.a(1), c.a(2)));
    reg("KERNEL._llseek", |c| call32(c.cpu, "kernel32!SetFilePointer", &[c.a(0) & 0xffff, c.a(1), 0, c.a(2) & 0xffff]));
    reg("KERNEL.OpenFile", open_file);
    reg("KERNEL.DOS3Call", |c| {
        dos(c.cpu);
        0
    });
    reg("KERNEL.GetCurrentTask", |_| 0x1007);
    reg("KERNEL.Yield", |_| 0);
    reg("KERNEL.GetWindowsDirectory", |c| crate::mem::put_cstr(c.a(0), c.a(1) & 0xffff, b"C:\\WINDOWS"));
    reg("KERNEL.GetSystemDirectory", |c| crate::mem::put_cstr(c.a(0), c.a(1) & 0xffff, b"C:\\WINDOWS\\SYSTEM"));
    reg("KERNEL.SetErrorMode", |_| 0);
    reg("WIN87EM.__fpMath", fp_math);
    reg("MMSYSTEM.waveOutOpen", wave_open);
    reg("MMSYSTEM.waveOutGetDevCaps", wave_caps);
    reg("MMSYSTEM.waveOutGetPosition", wave_position);
    reg("MMSYSTEM.waveOutPrepareHeader", |c| wave_header(c, "winmm!waveOutPrepareHeader"));
    reg("MMSYSTEM.waveOutUnprepareHeader", |c| wave_header(c, "winmm!waveOutUnprepareHeader"));
    reg("MMSYSTEM.waveOutWrite", |c| wave_header(c, "winmm!waveOutWrite"));
    reg("KERNEL.GetProcAddress", |c| {
        let (h, p) = (c.w(0), c.a(1));
        let h = if h == 0 { task().hinst } else { h };
        let e = if p < 0x10000 { None } else { Some(crate::text::a2s(&cstr(p))) };
        let e = match &e {
            None => w16::Export::Ordinal(p as u16),
            Some(n) => w16::Export::Name(n),
        };
        w16::proc_address(h, e).unwrap_or(0)
    });
    reg("KERNEL.LoadLibrary", |c| {
        let n = crate::text::a2s(&cstr(c.a(0)));
        w16::load_library(c.cpu, &n) as u32
    });
    reg("KERNEL.FreeLibrary", |c| {
        w16::free_library(c.cpu, c.w(0));
        0
    });
    reg("KERNEL.AnsiUpper", |c| ansi_case(c, true));
    reg("KERNEL.AnsiLower", |c| ansi_case(c, false));
    reg("USER.AnsiUpper", |c| ansi_case(c, true));
    reg("USER.AnsiLower", |c| ansi_case(c, false));
    reg("USER.AnsiNext", |c| {
        let p = lin(c.a(0));
        if crate::mem::r8(p) == 0 {
            c.a(0)
        } else if crate::text::is_lead(crate::mem::r8(p)) && crate::mem::r8(p + 1) != 0 {
            c.a(0) + 2
        } else {
            c.a(0) + 1
        }
    });
    reg("USER.AnsiPrev", |c| {
        let (s, p) = (lin(c.a(0)), lin(c.a(1)));
        let mut q = s;
        let mut prev = s;
        while q < p {
            prev = q;
            q += if crate::text::is_lead(crate::mem::r8(q)) { 2 } else { 1 };
        }
        c.a(1).wrapping_sub(p - prev)
    });
    // USER
    reg("USER.InitApp", |_| 1);
    reg("USER.RegisterClass", register_class);
    reg("USER.CreateWindow", |c| create_window(c, false));
    reg("USER.CreateWindowEx", |c| create_window(c, true));
    reg("USER.DefWindowProc", def_window_proc);
    reg("USER.CallWindowProc", call_window_proc);
    reg("USER.SendMessage", |c| send_post(c, "user32!SendMessageA"));
    reg("USER.PostMessage", |c| send_post(c, "user32!PostMessageA"));
    reg("USER.GetMessage", get_message);
    reg("USER.PeekMessage", peek_message);
    reg("USER.TranslateMessage", |c| {
        let m = msg_in(c.a(0));
        let r = call32(c.cpu, "user32!TranslateMessage", &[m.0]);
        msg_done(m);
        r
    });
    reg("USER.DispatchMessage", |c| {
        let m = msg_in(c.a(0));
        let r = call32(c.cpu, "user32!DispatchMessageA", &[m.0]);
        msg_done(m);
        r
    });
    reg("USER.BeginPaint", |c| {
        let ps = crate::mem::heap_alloc(64, true);
        let hdc = call32(c.cpu, "user32!BeginPaint", &[c.h(0), ps]);
        let p = c.a(1);
        w16(p, h16(r32(ps)));
        w16(p + 2, r32(ps + 4) as u16);
        rect_out(ps + 8, p + 4);
        crate::mem::heap_free(ps);
        h16(hdc) as u32
    });
    reg("USER.EndPaint", |c| {
        let ps = crate::mem::heap_alloc(64, true);
        let p = c.a(1);
        w32(ps, h32(r16(p)));
        w32(ps + 4, r16(p + 2) as u32);
        rect_in(p + 4, ps + 8);
        let r = call32(c.cpu, "user32!EndPaint", &[c.h(0), ps]);
        crate::mem::heap_free(ps);
        r
    });
    reg("USER.SetTimer", |c| {
        let proc = c.a(3);
        let p32 = if proc == 0 { 0 } else { proc_thunk(proc, Kind::Timer) };
        call32(c.cpu, "user32!SetTimer", &[c.h(0), c.w(1) as u32, c.w(2) as u32, p32])
    });
    reg("USER.LoadCursor", |c| {
        let id = if c.w(0) == 0 { c.a(1) } else { 32512 };
        h16(call32(c.cpu, "user32!LoadCursorA", &[0, id])) as u32
    });
    reg("USER.LoadIcon", |c| {
        let id = if c.w(0) == 0 { c.a(1) } else { 32512 };
        h16(call32(c.cpu, "user32!LoadIconA", &[0, id])) as u32
    });
    reg("USER.LoadBitmap", |c| {
        let name = c.a(1);
        if c.w(0) == 0 {
            // (the system's: OBM_*)
            return h16(call32(c.cpu, "user32!LoadBitmapA", &[0, name & 0xffff])) as u32;
        }
        let Some((d, len)) = find_resource(2, if name < 0x10000 { name | 0x8000_0000 } else { name }) else {
            return 0;
        };
        let h = bitmap_from_dib(c.cpu, d, len);
        if h == 0 {
            eprintln!("sake w16: LoadBitmap: not a bitmap ({len} bytes: {:02x?})", crate::mem::slice(d, len.min(16)));
        }
        crate::mem::heap_free(d);
        h16(h) as u32
    });
    reg("USER.LoadMenu", |c| {
        let p = c.a(1);
        h16(load_menu(c.cpu, p)) as u32
    });
    reg("USER.LoadString", |c| {
        let id = c.w(1) as u32;
        let (buf, n) = (c.a(2), c.s(3));
        let mut s = vec![];
        if let Some((d, len)) = find_resource(6, (id >> 4) + 1 | 0x8000_0000) {
            let mut p = d;
            for i in 0..16 {
                let l = crate::mem::r8(p) as u32;
                if i == id & 15 {
                    s = crate::mem::slice(p + 1, l.min(len)).to_vec();
                }
                p += l + 1;
            }
        }
        if n <= 0 {
            return 0;
        }
        crate::mem::put_cstr(buf, n as u32, &s)
    });
    reg("USER.DialogBox", |c| dialog_box(c, 0));
    reg("USER.DialogBoxParam", |c| {
        let p = c.a(4);
        dialog_box(c, p)
    });
    reg("USER._wsprintf", |c| {
        let s = format16(c.a(1), c.va);
        let d = c.a(0);
        crate::mem::write(d, &s);
        w8(d + s.len() as u32, 0);
        s.len() as u32
    });
    reg("USER.wvsprintf", |c| {
        let s = format16(c.a(1), lin(c.a(2)));
        let d = c.a(0);
        crate::mem::write(d, &s);
        w8(d + s.len() as u32, 0);
        s.len() as u32
    });
    reg("USER.GetWindowWord", |c| {
        let r = match c.s(1) {
            -6 => task().hinst as u32, // GWW_HINSTANCE
            -8 => h16(call32(c.cpu, "user32!GetParent", &[c.h(0)])) as u32,
            -12 => call32(c.cpu, "user32!GetWindowLongA", &[c.h(0), (-12i32) as u32]),
            i => call32(c.cpu, "user32!GetWindowLongA", &[c.h(0), i as u32]),
        };
        r & 0xffff
    });
    reg("USER.SetWindowWord", |c| call32(c.cpu, "user32!SetWindowLongA", &[c.h(0), c.s(1) as u32, c.w(2) as u32]));
    reg("USER.GetWindowLong", |c| {
        let i = c.s(1);
        let r = call32(c.cpu, "user32!GetWindowLongA", &[c.h(0), i as u32]);
        if i == -4 { proc_back(r) } else { r }
    });
    reg("USER.SetWindowLong", |c| {
        let i = c.s(1);
        let v = if i == -4 { proc_thunk(c.a(2), Kind::Wnd) } else { c.a(2) };
        let r = call32(c.cpu, "user32!SetWindowLongA", &[c.h(0), i as u32, v]);
        if i == -4 { proc_back(r) } else { r }
    });
    reg("USER.GetClassWord", |c| call32(c.cpu, "user32!GetClassLongA", &[c.h(0), c.s(1) as u32]) & 0xffff);
    reg("USER.GetClassLong", |c| {
        let i = c.s(1);
        let r = call32(c.cpu, "user32!GetClassLongA", &[c.h(0), i as u32]);
        if i == -24 { proc_back(r) } else { r } // GCL_WNDPROC
    });
    reg("USER.SetClassLong", |c| {
        let i = c.s(1);
        let v = if i == -24 { proc_thunk(c.a(2), Kind::Wnd) } else { c.a(2) };
        let r = call32(c.cpu, "user32!SetClassLongA", &[c.h(0), i as u32, v]);
        if i == -24 { proc_back(r) } else { r }
    });
    reg("USER.GetDesktopWindow", |c| h16(call32(c.cpu, "user32!GetDesktopWindow", &[])) as u32);
    reg("USER.DrawText", |c| {
        let r = crate::mem::heap_alloc(16, true);
        rect_in(c.a(3), r);
        let v = call32(c.cpu, "user32!DrawTextA", &[c.h(0), c.a(1), c.s(2) as u32, r, c.w(4) as u32]);
        rect_out(r, c.a(3));
        crate::mem::heap_free(r);
        v
    });
    reg("USER.GetUpdateRect", |c| {
        let r = crate::mem::heap_alloc(16, true);
        let v = call32(c.cpu, "user32!GetUpdateRect", &[c.h(0), if c.a(1) == 0 { 0 } else { r }, c.w(2) as u32]);
        if c.a(1) != 0 {
            rect_out(r, c.a(1));
        }
        crate::mem::heap_free(r);
        v
    });
    reg("USER.SetRect", |c| {
        let p = c.a(0);
        for i in 0..4 {
            w16(p + 2 * i as u32, c.w(1 + i));
        }
        1
    });
    reg("USER.SetRectEmpty", |c| {
        crate::mem::fill(c.a(0), 8, 0);
        1
    });
    reg("USER.CopyRect", |c| {
        crate::mem::copy(c.a(0), c.a(1), 8);
        1
    });
    reg("USER.PtInRect", |c| {
        let p = c.a(0);
        let pt = c.a(1);
        let (x, y) = (pt as u16 as i16, (pt >> 16) as u16 as i16);
        let r = |i: u32| r16(p + 2 * i) as i16;
        (x >= r(0) && x < r(2) && y >= r(1) && y < r(3)) as u32
    });
    reg("USER.OffsetRect", |c| {
        let p = c.a(0);
        let (dx, dy) = (c.w(1), c.w(2));
        for (i, d) in [(0, dx), (1, dy), (2, dx), (3, dy)] {
            w16(p + 2 * i, r16(p + 2 * i).wrapping_add(d));
        }
        1
    });
    reg("USER.InflateRect", |c| {
        let p = c.a(0);
        let (dx, dy) = (c.w(1), c.w(2));
        w16(p, r16(p).wrapping_sub(dx));
        w16(p + 2, r16(p + 2).wrapping_sub(dy));
        w16(p + 4, r16(p + 4).wrapping_add(dx));
        w16(p + 6, r16(p + 6).wrapping_add(dy));
        1
    });
    reg("USER.IsRectEmpty", |c| {
        let p = c.a(0);
        (r16(p + 4) as i16 <= r16(p) as i16 || r16(p + 6) as i16 <= r16(p + 2) as i16) as u32
    });
    reg("USER.EndDialog", |c| call32(c.cpu, "user32!EndDialog", &[c.h(0), c.s(1) as u32]));
    reg("USER.GetSystemMenu", |c| h16(call32(c.cpu, "user32!GetSystemMenu", &[c.h(0), c.w(1) as u32])) as u32);
    reg("USER.GetTextMetrics", |c| text_metrics(c));
    reg("GDI.GetTextMetrics", |c| text_metrics(c));
    reg("GDI.DeleteObject", |c| {
        let r = call32(c.cpu, "gdi32!DeleteObject", &[c.h(0)]);
        w16::h_release(c.w(0));
        r
    });
    reg("GDI.GetTextExtent", |c| {
        let sz = crate::mem::heap_alloc(8, true);
        call32(c.cpu, "gdi32!GetTextExtentPoint32A", &[c.h(0), c.a(1), c.s(2) as u32, sz]);
        let r = (r32(sz) & 0xffff) | (r32(sz + 4) << 16);
        crate::mem::heap_free(sz);
        r
    });
    reg("GDI.MoveTo", |c| {
        let pt = crate::mem::heap_alloc(8, true);
        call32(c.cpu, "gdi32!MoveToEx", &[c.h(0), c.s(1) as u32, c.s(2) as u32, pt]);
        let r = (r32(pt) & 0xffff) | (r32(pt + 4) << 16);
        crate::mem::heap_free(pt);
        r
    });
    reg("GDI.GetObject", |c| get_object(c));
    reg("GDI.CreateFont", |c| {
        let lf = crate::mem::heap_alloc(60, true);
        for i in 0..5 {
            w32(lf + 4 * i as u32, c.s(i) as u32);
        }
        for i in 0..8 {
            w8(lf + 20 + i as u32, c.w(5 + i) as u8);
        }
        if c.a(13) != 0 {
            let s = cstr(c.a(13));
            crate::mem::put_cstr(lf + 28, 32, &s);
        }
        let r = call32(c.cpu, "gdi32!CreateFontIndirectA", &[lf]);
        crate::mem::heap_free(lf);
        h16(r) as u32
    });
    reg("GDI.CreateBitmap", |c| {
        let bits = if c.a(4) == 0 { 0 } else { c.a(4) };
        h16(call32(c.cpu, "gdi32!CreateBitmap", &[c.s(0) as u32, c.s(1) as u32, c.w(2) as u32, c.w(3) as u32, bits])) as u32
    });
    reg("COMMDLG.GetOpenFileName", |c| open_file_name(c));
    reg("KEYBOARD.AnsiToOem", |c| {
        let s = cstr(c.a(0));
        let d = c.a(1);
        crate::mem::write(d, &s);
        w8(d + s.len() as u32, 0);
        1
    });
}

fn ansi_case(c: &mut C16, up: bool) -> u32 {
    let v = c.a(0);
    if v >> 16 == 0 {
        let b = [v as u8];
        let mut s = b.to_vec();
        if up { crate::text::upper_a(&mut s) } else { crate::text::lower_a(&mut s) }
        return s[0] as u32;
    }
    let p = lin(v);
    let mut s = cstr(p);
    if up { crate::text::upper_a(&mut s) } else { crate::text::lower_a(&mut s) }
    crate::mem::write(p, &s);
    v
}

fn file_read(c: &mut C16, h: u32, buf: u32, n: u32) -> u32 {
    let got = crate::mem::heap_alloc(4, true);
    let ok = call32(c.cpu, "kernel32!ReadFile", &[h, buf, n, got, 0]);
    let r = r32(got);
    crate::mem::heap_free(got);
    if ok == 0 { 0xffff_ffff } else { r }
}

fn file_write(c: &mut C16, h: u32, buf: u32, n: u32) -> u32 {
    if let Some(n) = std_write(h, buf, n) {
        return n;
    }
    let got = crate::mem::heap_alloc(4, true);
    let ok = call32(c.cpu, "kernel32!WriteFile", &[h, buf, n, got, 0]);
    let r = r32(got);
    crate::mem::heap_free(got);
    if ok == 0 { 0xffff_ffff } else { r }
}

/// DOS handles 1 and 2 (stdout, stderr: _lwrite or INT 21h's write) go to the console, a line at
/// a time; `None` for other handles
fn std_write(h: u32, buf: u32, n: u32) -> Option<u32> {
    if h != 1 && h != 2 {
        return None;
    }
    let b: Vec<u8> = (0..n).map(|i| crate::mem::r8(buf + i)).collect();
    crate::k32::console_write(&crate::text::a2s(&b));
    Some(n)
}

/// OpenFile(name, OFSTRUCT*, mode): OFSTRUCT16 has the same layout as the 32-bit one.
fn open_file(c: &mut C16) -> u32 {
    let r = call32(c.cpu, "kernel32!OpenFile", &[c.a(0), c.a(1), c.w(2) as u32]);
    if r == u32::MAX { 0xffff } else { r & 0xffff }
}

thread_local! {
    /// Catch/Throw's registers for after the return: [ip, cs, sp, bp, si, di, ds, ss, ax]
    static JUMP: std::cell::Cell<[u16; 9]> = const { std::cell::Cell::new([0; 9]) };
}

/// waveOutOpen(&hwo, device, format, callback, instance, flags) on winmm's: WAVE_MAPPER is
/// 0xffff here, a window callback a 16-bit window, a task's the program's thread; function
/// callbacks (called at interrupt time on Windows 3.1) are not delivered.
fn wave_open(c: &mut C16) -> u32 {
    let (ph, dev, fmt, cb, inst, flags) = (c.a(0), c.w(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let dev = if dev == 0xffff { u32::MAX } else { dev as u32 };
    let (flags, cb) = match flags & 0x70000 {
        0x10000 => (flags, h32(cb as u16)),
        0x20000 => (flags, call32(c.cpu, "kernel32!GetCurrentThreadId", &[])),
        0 => (flags, cb),
        k => {
            eprintln!("sake w16: waveOutOpen callback kind {k:x} not delivered");
            (flags & !0x70000, 0)
        }
    };
    let m = w16::scratch_mark();
    let (_, tmp) = w16::scratch(4);
    let r = call32(c.cpu, "winmm!waveOutOpen", &[if ph == 0 { 0 } else { tmp }, dev, fmt, cb, inst, flags]);
    let h = r32(tmp);
    w16::scratch_reset(m);
    if r == 0 && h != 0 {
        crate::sound::wave16(h);
    }
    if ph != 0 && flags & 1 == 0 {
        w16(ph, h as u16);
    }
    r
}

/// waveOutPrepareHeader / UnprepareHeader / Write(hwo, WAVEHDR far*, size): the 16-bit
/// WAVEHDR is the 32-bit one but for lpData (far), which the device reads as such.
fn wave_header(c: &mut C16, f: &str) -> u32 {
    let (hwo, far) = (c.w(0) as u32, c.a(1));
    let l = lin(far);
    if l != 0 {
        crate::sound::wave16_header(hwo, l, far);
    }
    call32(c.cpu, f, &[hwo, l, 32])
}

/// waveOutGetDevCaps(device, WAVEOUTCAPS16*, size): 16-bit vDriverVersion, no wReserved1.
fn wave_caps(c: &mut C16) -> u32 {
    let (dev, p, size) = (c.w(0), c.a(1), c.w(2) as u32);
    let dev = if dev == 0xffff { u32::MAX } else { dev as u32 };
    let m = w16::scratch_mark();
    let (_, t) = w16::scratch(52);
    let r = call32(c.cpu, "winmm!waveOutGetDevCapsA", &[dev, t, 52]);
    if r == 0 && p != 0 {
        let s = crate::mem::slice(t, 52).to_vec();
        let mut o = Vec::with_capacity(48);
        o.extend(&s[0..6]); // wMid, wPid, vDriverVersion (low word)
        o.extend(&s[8..46]); // szPname, dwFormats, wChannels
        o.extend(&s[48..52]); // dwSupport
        crate::mem::write(p, &o[..size.min(48) as usize]);
    }
    w16::scratch_reset(m);
    r
}

/// waveOutGetPosition(hwo, MMTIME16*, size): a 16-bit wType, the union after it.
fn wave_position(c: &mut C16) -> u32 {
    let (hwo, p, size) = (c.w(0) as u32, c.a(1), c.w(2) as u32);
    if p == 0 || size < 6 {
        return 11; // MMSYSERR_INVALPARAM
    }
    let m = w16::scratch_mark();
    let (_, t) = w16::scratch(12);
    w32(t, r16(p) as u32);
    let r = call32(c.cpu, "winmm!waveOutGetPosition", &[hwo, t, 12]);
    if r == 0 {
        w16(p, r32(t) as u16);
        crate::mem::write(p + 2, &crate::mem::slice(t + 4, 8).to_vec()[..(size - 2).min(8) as usize]);
    }
    w16::scratch_reset(m);
    r
}

/// WIN87EM's __fpMath (a register function, BX the request). There is a coprocessor
/// (GetWinFlags' WF_80x87, so the OSFIXUPs stay unapplied): 0 initialize and 1 reset are
/// FNINIT, 2 terminate nothing, 3 records the program's exception handler (DX:AX); AX 0.
fn fp_math(c: &mut C16) -> u32 {
    let cpu = &mut *c.cpu;
    let bx = cpu.regs[koji::EBX] as u16;
    match bx {
        0 | 1 => cpu.fpu = Default::default(),
        2 => {}
        3 => *FP_HANDLER.lock().unwrap() = (cpu.regs[EDX] & 0xffff) << 16 | cpu.regs[EAX] & 0xffff,
        _ => eprintln!("sake w16: __fpMath {bx}"),
    }
    cpu.regs[EAX] &= 0xffff_0000;
    0
}

/// the program's floating point exception handler (__fpMath 3)
static FP_HANDLER: Mutex<u32> = Mutex::new(0);

/// Catch(CATCHBUF*): the caller's registers as they will be after this call returns, so that
/// Throw can return there again (Windows' setjmp; Free Pascal's RTL uses it for Halt and
/// exceptions). CATCHBUF: ip, cs, sp, bp, si, di, ds, ss, and a spare word. Returns 0.
fn catch(c: &mut C16) -> u32 {
    let buf = c.args[0];
    let cpu = &*c.cpu;
    // (the far return address, then the pointer argument: both gone after the return)
    let sp = ((cpu.regs[ESP] & 0xffff) + 8) as u16;
    let regs = [r16(c.sp), r16(c.sp + 2), sp, cpu.regs[koji::EBP] as u16, cpu.regs[koji::ESI] as u16, cpu.regs[koji::EDI] as u16, cpu.sreg[3], cpu.sreg[2], 0];
    for (i, v) in regs.iter().enumerate() {
        w16(buf + 2 * i as u32, *v);
    }
    JUMP.with(|j| j.set(regs));
    w16::AFTER.with(|a| a.set(Some(jump)));
    0
}

/// Throw(CATCHBUF*, value): back to where Catch returned, with value as its result.
fn throw(c: &mut C16) -> u32 {
    let buf = c.args[0];
    let mut regs = [0u16; 9];
    for (i, v) in regs.iter_mut().enumerate().take(8) {
        *v = r16(buf + 2 * i as u32);
    }
    regs[8] = c.args[1] as u16;
    JUMP.with(|j| j.set(regs));
    w16::AFTER.with(|a| a.set(Some(jump)));
    0
}

/// Catch's and Throw's return (a "register" function: dispatch only popped the return address)
fn jump(cpu: &mut Cpu) {
    let [ip, cs, sp, bp, si, di, ds, ss, ax] = JUMP.with(|j| j.get());
    let set = |r: &mut u32, v: u16| *r = (*r & 0xffff_0000) | v as u32;
    set(&mut cpu.regs[ESP], sp);
    set(&mut cpu.regs[koji::EBP], bp);
    set(&mut cpu.regs[koji::ESI], si);
    set(&mut cpu.regs[koji::EDI], di);
    set(&mut cpu.regs[EAX], ax);
    w16::set_seg(cpu, 3, ds);
    w16::set_seg(cpu, 2, ss);
    w16::set_seg(cpu, 1, cs);
    cpu.eip = ip as u32;
}

fn init_task(c: &mut C16) -> u32 {
    let cpu = &mut *c.cpu;
    let ds = cpu.sreg[3];
    let base = cpu.sbase[3];
    let sp = (cpu.regs[ESP] & 0xffff) + 4;
    let bx = cpu.regs[3] & 0xffff;
    w16(base + 0x0c, sp as u16); // stackmin
    w16(base + 0x0e, sp as u16); // stackbottom
    w16(base + 0x0a, (sp.saturating_sub(bx) + 150) as u16); // stacktop
    let t = task();
    let pb = w16::sel_base(t.psp).unwrap().0;
    let mut bxv = 0x81;
    while matches!(crate::mem::r8(pb + bxv), b' ' | b'\t') {
        bxv += 1;
    }
    cpu.regs[EAX] = 1;
    cpu.regs[3] = bxv;
    cpu.regs[ECX] = r16(base + 0x0a) as u32;
    cpu.regs[EDX] = t.cmd_show as u32;
    cpu.regs[6] = 0;
    cpu.regs[7] = t.hinst as u32;
    let _ = ds;
    w16::AFTER.with(|a| {
        a.set(Some(|cpu: &mut Cpu| {
            // InitTask leaves a zero word (BP) on the stack
            let sp = (cpu.regs[ESP] & 0xffff).wrapping_sub(2);
            w16(cpu.sbase[2] + sp, 0);
            cpu.regs[ESP] = (cpu.regs[ESP] & 0xffff_0000) | sp;
            let psp = task().psp;
            w16::set_seg(cpu, 0, psp);
        }))
    });
    0
}

// ---------------------------------------------------------------------------------------------
// window procedures

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Wnd,
    Dlg,
    Timer,
}

struct Procs {
    /// 32-bit thunk -> (16-bit procedure, kind)
    by_trap: HashMap<u32, (u32, Kind)>,
    by_fp: HashMap<(u32, u8), u32>,
}

static PROCS: Mutex<Option<Procs>> = Mutex::new(None);

/// A 32-bit callable address for the 16-bit procedure `fp`.
pub fn proc_thunk(fp: u32, kind: Kind) -> u32 {
    if fp == 0 {
        return 0;
    }
    if let Some(p32) = proc32_of(fp) {
        return p32;
    }
    let mut g = PROCS.lock().unwrap();
    let p = g.get_or_insert_with(|| Procs { by_trap: HashMap::default(), by_fp: HashMap::default() });
    if let Some(&t) = p.by_fp.get(&(fp, kind as u8)) {
        return t;
    }
    let t = crate::api::host_fn(&format!("w16proc{fp:08x}_{}", kind as u8), proc_entry, 16);
    p.by_trap.insert(t, (fp, kind));
    p.by_fp.insert((fp, kind as u8), t);
    t
}

/// The 16-bit procedure behind a 32-bit one (GetWindowLong(GWL_WNDPROC)): our thunk's, or
/// one that calls the 32-bit procedure.
pub fn proc_back(p32: u32) -> u32 {
    if let Some(&(fp, _)) = PROCS.lock().unwrap().as_ref().and_then(|p| p.by_trap.get(&p32)) {
        return fp;
    }
    proc16_of(p32)
}

static P32: Mutex<Option<(HashMap<u32, u32>, HashMap<u32, u32>)>> = Mutex::new(None);

/// A far pointer that calls the 32-bit procedure `p32`: CallWindowProc recognizes it, and a
/// program calling it itself (an old window procedure kept from a subclassing) reaches it too.
fn proc16_of(p32: u32) -> u32 {
    let mut g = P32.lock().unwrap();
    let m = g.get_or_insert_with(Default::default);
    if let Some(&fp) = m.0.get(&p32) {
        return fp;
    }
    let fp = w16::host16("USER", format!("{p32:08x}"), "wwwl", call_proc32);
    m.0.insert(p32, fp);
    m.1.insert(fp, p32);
    fp
}

/// A program's call of a 32-bit window procedure (proc16_of): its message made 32-bit.
fn call_proc32(c: &mut C16) -> u32 {
    let p32 = u32::from_str_radix(&c.name, 16).unwrap();
    let (m, w, l, tmp) = to32(c.w(1), c.w(2), c.a(3));
    let r = crate::call(c.cpu, p32, &[c.h(0), m, w, l]);
    to32_done(tmp);
    r
}

fn proc32_of(fp: u32) -> Option<u32> {
    P32.lock().unwrap().as_ref().and_then(|m| m.1.get(&fp).copied())
}

fn proc_entry(c: &mut crate::api::Ctx) -> u32 {
    let Some((fp, kind)) = PROCS.lock().unwrap().as_ref().and_then(|p| p.by_trap.get(&c.trap).copied()) else {
        return 0;
    };
    let (hwnd, msg, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if kind == Kind::Timer {
        return call16(c.cpu, fp, &[A::W(h16(hwnd)), A::W(msg as u16), A::W(wp as u16), A::L(lp)]);
    }
    let mark = w16::scratch_mark();
    let (m16, wp16, lp16, back) = to16(msg, wp, lp);
    let r = call16(c.cpu, fp, &[A::W(h16(hwnd)), A::W(m16), A::W(wp16), A::L(lp16)]);
    let r = from16_done(back, r, kind);
    w16::scratch_reset(mark);
    r
}

/// How a 32-bit message's parameters reach 16-bit code, and what comes back.
pub enum Back {
    None,
    MinMax(u32, u32),
    CalcSize(u32, u32, bool),
    WinPos(u32, u32),
    /// (the 32-bit buffer, the scratch copy, the buffer's size in characters)
    Text(u32, u32, u32),
}

/// 32 -> 16: (message, wParam, lParam) for a 16-bit procedure (structs in scratch memory).
pub fn to16(msg: u32, wp: u32, lp: u32) -> (u16, u16, u32, Back) {
    let lo = |v: u32| v & 0xffff;
    let hi = |v: u32| v >> 16;
    match msg {
        // WM_CREATE, WM_NCCREATE: CREATESTRUCT16
        0x01 | 0x81 if lp != 0 => {
            let (fp, l) = w16::scratch(34);
            w32(l, r32(lp));
            w16(l + 4, task().hinst);
            w16(l + 6, h16(r32(lp + 8)));
            w16(l + 8, h16(r32(lp + 12)));
            for i in 0..4 {
                w16(l + 10 + 2 * i, r32(lp + 16 + 4 * i) as u16);
            }
            w32(l + 18, r32(lp + 32));
            w32(l + 22, w16::scratch_str(r32(lp + 36)));
            w32(l + 26, w16::scratch_str(r32(lp + 40)));
            w32(l + 30, r32(lp + 44));
            (msg as u16, wp as u16, fp, Back::None)
        }
        // WM_GETMINMAXINFO
        0x24 if lp != 0 => {
            let (fp, l) = w16::scratch(20);
            for i in 0..10 {
                w16(l + 2 * i, r32(lp + 4 * i) as u16);
            }
            (0x24, wp as u16, fp, Back::MinMax(lp, l))
        }
        // WM_NCCALCSIZE
        0x83 if lp != 0 => {
            let n = if wp != 0 { 3 } else { 1 };
            let (fp, l) = w16::scratch(8 * n + 4);
            for i in 0..4 * n {
                w16(l + 2 * i, r32(lp + 4 * i) as u16);
            }
            (0x83, wp as u16, fp, Back::CalcSize(lp, l, wp != 0))
        }
        // WM_WINDOWPOSCHANGING/CHANGED
        0x46 | 0x47 if lp != 0 => {
            let (fp, l) = w16::scratch(14);
            w16(l, h16(r32(lp)));
            w16(l + 2, h16(r32(lp + 4)));
            for i in 0..5 {
                w16(l + 4 + 2 * i, r32(lp + 8 + 4 * i) as u16);
            }
            (msg as u16, wp as u16, fp, Back::WinPos(lp, l))
        }
        // WM_SETTEXT
        0x0c if lp != 0 => (0x0c, wp as u16, w16::scratch_str(lp), Back::None),
        // WM_GETTEXT
        0x0d if lp != 0 => {
            let n = wp.min(0x4000);
            let (fp, l) = w16::scratch(n + 1);
            (0x0d, n as u16, fp, Back::Text(lp, l, n))
        }
        // WM_COMMAND: (id, MAKELONG(hwnd, code))
        0x111 => (0x111, lo(wp) as u16, (h16(lp) as u32) | hi(wp) << 16, Back::None),
        // WM_ACTIVATE
        0x06 => (0x06, lo(wp) as u16, (h16(lp) as u32) | hi(wp) << 16, Back::None),
        // WM_HSCROLL, WM_VSCROLL: (code, MAKELONG(pos, hwnd))
        0x114 | 0x115 => (msg as u16, lo(wp) as u16, hi(wp) | (h16(lp) as u32) << 16, Back::None),
        // WM_MENUSELECT: (item, MAKELONG(flags, hmenu))
        0x11f => (0x11f, lo(wp) as u16, hi(wp) | (h16(lp) as u32) << 16, Back::None),
        // WM_CTLCOLOR*: WM_CTLCOLOR (hdc, MAKELONG(hwnd, type))
        0x132..=0x138 => (0x19, h16(wp), (h16(lp) as u32) | (msg - 0x132) << 16, Back::None),
        // WM_PARENTNOTIFY
        0x210 => (0x210, lo(wp) as u16, if lo(wp) == 1 || lo(wp) == 2 { (h16(lp) as u32) | hi(wp) << 16 } else { lp }, Back::None),
        // messages whose wParam is a handle
        0x0f | 0x14 | 0x07 | 0x08 | 0x20 | 0x0e => (msg as u16, if matches!(msg, 0x14 | 0x07 | 0x08 | 0x20) { h16(wp) } else { wp as u16 }, lp, Back::None),
        _ => (msg as u16, wp as u16, lp, Back::None),
    }
}

/// After a 16-bit procedure handled a converted message: copy results back; the LRESULT.
pub fn from16_done(back: Back, r: u32, kind: Kind) -> u32 {
    match back {
        Back::MinMax(lp, l) => {
            for i in 0..10 {
                w32(lp + 4 * i, r16(l + 2 * i) as i16 as i32 as u32);
            }
        }
        Back::CalcSize(lp, l, all) => {
            for i in 0..if all { 12 } else { 4 } {
                w32(lp + 4 * i, r16(l + 2 * i) as i16 as i32 as u32);
            }
        }
        Back::WinPos(lp, l) => {
            for i in 0..5 {
                w32(lp + 8 + 4 * i, r16(l + 4 + 2 * i) as i16 as i32 as u32);
            }
        }
        Back::Text(lp, l, cap) => {
            // (at most the caller's buffer, wParam characters with the NUL)
            if cap > 0 {
                let s = cstr(l);
                let n = s.len().min(cap as usize - 1);
                crate::mem::write(lp, &s[..n]);
                w8(lp + n as u32, 0);
            }
        }
        Back::None => {}
    }
    match kind {
        Kind::Dlg => r & 0xffff,
        _ => r,
    }
}

/// 16 -> 32 for messages a 16-bit program sends or passes on (DefWindowProc, SendMessage):
/// (wParam, lParam, cleanup).
pub fn to32(msg: u16, wp: u16, lp: u32) -> (u32, u32, u32, Vec<(u32, u32, u8)>) {
    let mut tmp = vec![];
    let (m, w, l) = match msg {
        0x01 | 0x81 if lp != 0 => {
            let s = lin(lp);
            let b = crate::mem::heap_alloc(48, true);
            w32(b, r32(s));
            w32(b + 4, 0);
            w32(b + 8, h32(r16(s + 6)));
            w32(b + 12, h32(r16(s + 8)));
            for i in 0..4 {
                w32(b + 16 + 4 * i, r16(s + 10 + 2 * i) as i16 as i32 as u32);
            }
            w32(b + 32, r32(s + 18));
            w32(b + 36, lin(r32(s + 22)));
            w32(b + 40, lin(r32(s + 26)));
            w32(b + 44, r32(s + 30));
            tmp.push((b, s, b'c'));
            (msg as u32, wp as u32, b)
        }
        0x24 if lp != 0 => {
            let s = lin(lp);
            let b = crate::mem::heap_alloc(40, true);
            for i in 0..10 {
                w32(b + 4 * i, r16(s + 2 * i) as i16 as i32 as u32);
            }
            tmp.push((b, s, b'm'));
            (0x24, wp as u32, b)
        }
        0x83 if lp != 0 => {
            let s = lin(lp);
            let b = crate::mem::heap_alloc(64, true);
            let n = if wp != 0 { 12 } else { 4 };
            for i in 0..n {
                w32(b + 4 * i, r16(s + 2 * i) as i16 as i32 as u32);
            }
            if wp != 0 {
                let wpos = crate::mem::heap_alloc(28, true);
                w32(b + 48, wpos);
            }
            tmp.push((b, s, if wp != 0 { b'C' } else { b'n' }));
            (0x83, wp as u32, b)
        }
        0x46 | 0x47 if lp != 0 => {
            let s = lin(lp);
            let b = crate::mem::heap_alloc(28, true);
            w32(b, h32(r16(s)));
            w32(b + 4, h32(r16(s + 2)));
            for i in 0..5 {
                w32(b + 8 + 4 * i, r16(s + 4 + 2 * i) as i16 as i32 as u32);
            }
            tmp.push((b, s, b'w'));
            (msg as u32, wp as u32, b)
        }
        0x0c | 0x0d if lp != 0 => (msg as u32, wp as u32, lin(lp)),
        0x111 => (0x111, wp as u32 | (lp >> 16) << 16, h32(lp as u16), ),
        0x06 => (0x06, wp as u32 | (lp >> 16) << 16, h32(lp as u16)),
        0x114 | 0x115 => (msg as u32, wp as u32 | (lp & 0xffff) << 16, h32((lp >> 16) as u16)),
        0x11f => (0x11f, wp as u32 | (lp & 0xffff) << 16, h32((lp >> 16) as u16)),
        0x19 => (0x132 + (lp >> 16), h32(wp), h32(lp as u16)),
        0x14 | 0x07 | 0x08 | 0x20 => (msg as u32, h32(wp), lp),
        // (WM_USER and above, and the rest: as they are)
        _ => (msg as u32, wp as u32, lp),
    };
    (m, w, l, tmp)
}

pub fn to32_done(tmp: Vec<(u32, u32, u8)>) {
    for (b, s, k) in tmp {
        match k {
            b'm' => {
                for i in 0..10 {
                    w16(s + 2 * i, r32(b + 4 * i) as u16);
                }
            }
            b'n' | b'C' => {
                for i in 0..if k == b'C' { 12 } else { 4 } {
                    w16(s + 2 * i, r32(b + 4 * i) as u16);
                }
                if k == b'C' {
                    crate::mem::heap_free(r32(b + 48));
                }
            }
            b'w' => {
                for i in 0..5 {
                    w16(s + 4 + 2 * i, r32(b + 8 + 4 * i) as u16);
                }
            }
            _ => {}
        }
        crate::mem::heap_free(b);
    }
}

fn def_window_proc(c: &mut C16) -> u32 {
    let (m, w, l, tmp) = to32(c.w(1), c.w(2), c.a(3));
    let r = call32(c.cpu, "user32!DefWindowProcA", &[c.h(0), m, w, l]);
    to32_done(tmp);
    r
}

fn send_post(c: &mut C16, func: &str) -> u32 {
    let (m, w, l, tmp) = to32(c.w(1), c.w(2), c.a(3));
    let h = if c.w(0) == 0xffff { 0xffff } else { c.h(0) };
    let r = call32(c.cpu, func, &[h, m, w, l]);
    to32_done(tmp);
    r
}

fn call_window_proc(c: &mut C16) -> u32 {
    let fp = c.a(0);
    match proc32_of(fp) {
        Some(p32) => {
            let (m, w, l, tmp) = to32(c.w(2), c.w(3), c.a(4));
            let r = crate::call(c.cpu, p32, &[c.h(1), m, w, l]);
            to32_done(tmp);
            r
        }
        None => call16(c.cpu, fp, &[A::W(c.w(1)), A::W(c.w(2)), A::W(c.w(3)), A::L(c.a(4))]),
    }
}

/// MSG16 at `p` (linear) -> a new 32-bit MSG (free it with heap_free).
/// A 16-bit MSG as a 32-bit one (a new block) and the blocks its lParam's conversion made:
/// msg_done frees them all (to32's blocks were dropped and leaked).
fn msg_in(p: u32) -> (u32, Vec<(u32, u32, u8)>) {
    let m = crate::mem::heap_alloc(28, true);
    let (msg, wp, lp, tmp) = to32(r16(p + 2), r16(p + 4), r32(p + 6));
    w32(m, h32(r16(p)));
    w32(m + 4, msg);
    w32(m + 8, wp);
    w32(m + 12, lp);
    w32(m + 16, r32(p + 10));
    w32(m + 20, r16(p + 14) as i16 as i32 as u32);
    w32(m + 24, r16(p + 16) as i16 as i32 as u32);
    // (WM_TIMER: lParam back to the 32-bit thunk)
    if msg == 0x113 && r32(p + 6) != 0 {
        w32(m + 12, proc_thunk(r32(p + 6), Kind::Timer));
    }
    (m, tmp)
}
fn msg_done((m, tmp): (u32, Vec<(u32, u32, u8)>)) {
    to32_done(tmp);
    crate::mem::heap_free(m);
}

fn msg_out(m: u32, p: u32) {
    let (msg, wp, lp) = (r32(m + 4), r32(m + 8), r32(m + 12));
    let (m16, wp16, lp16, _) = to16(msg, wp, lp);
    w16(p, h16(r32(m)));
    w16(p + 2, m16);
    w16(p + 4, wp16);
    let lp16 = if msg == 0x113 && lp != 0 { proc_back(lp) } else { lp16 };
    w32(p + 6, lp16);
    w32(p + 10, r32(m + 16));
    w16(p + 14, r32(m + 20) as u16);
    w16(p + 16, r32(m + 24) as u16);
}

fn get_message(c: &mut C16) -> u32 {
    let m = crate::mem::heap_alloc(28, true);
    let r = call32(c.cpu, "user32!GetMessageA", &[m, c.h(1), c.w(2) as u32, c.w(3) as u32]);
    msg_out(m, c.a(0));
    crate::mem::heap_free(m);
    r
}

fn peek_message(c: &mut C16) -> u32 {
    let m = crate::mem::heap_alloc(28, true);
    let r = call32(c.cpu, "user32!PeekMessageA", &[m, c.h(1), c.w(2) as u32, c.w(3) as u32, c.w(4) as u32]);
    if r != 0 {
        msg_out(m, c.a(0));
    }
    crate::mem::heap_free(m);
    r
}

/// Classes registered by the program: name -> menu name (far pointer as given)
static CLASS_MENU: Mutex<Option<HashMap<Vec<u8>, u32>>> = Mutex::new(None);

fn register_class(c: &mut C16) -> u32 {
    let p = c.a(0);
    let wc = crate::mem::heap_alloc(40, true);
    w32(wc, r16(p) as u32);
    w32(wc + 4, proc_thunk(r32(p + 2), Kind::Wnd));
    w32(wc + 8, r16(p + 6) as i16 as i32 as u32);
    w32(wc + 12, r16(p + 8) as i16 as i32 as u32);
    w32(wc + 16, 0);
    w32(wc + 20, h32(r16(p + 12)));
    w32(wc + 24, h32(r16(p + 14)));
    let br = r16(p + 16) as u32;
    // (COLOR_x + 1 stays a number)
    w32(wc + 28, if br <= 0x20 { br } else { h32(br as u16) });
    w32(wc + 32, 0);
    let name = lin(r32(p + 22));
    w32(wc + 36, name);
    let menu = r32(p + 18);
    if menu != 0 {
        let key = if name < 0x10000 { name.to_le_bytes().to_vec() } else { cstr(name).to_ascii_uppercase() };
        CLASS_MENU.lock().unwrap().get_or_insert_with(HashMap::default).insert(key, menu);
    }
    let r = call32(c.cpu, "user32!RegisterClassA", &[wc]);
    crate::mem::heap_free(wc);
    r & 0xffff
}

fn create_window(c: &mut C16, ex: bool) -> u32 {
    let o = if ex { 1 } else { 0 };
    let exstyle = if ex { c.a(0) } else { 0 };
    let cls = c.a(o);
    let cw = |v: i32| if v == -32768 { 0x8000_0000 } else { v as u32 };
    let (x, y, w, h) = (cw(c.s(o + 3)), cw(c.s(o + 4)), cw(c.s(o + 5)), cw(c.s(o + 6)));
    let mut menu = c.h(o + 8);
    let style = c.a(o + 2);
    if menu == 0 && style & 0x4000_0000 == 0 {
        let key = if cls < 0x10000 { cls.to_le_bytes().to_vec() } else { cstr(cls).to_ascii_uppercase() };
        let m = CLASS_MENU.lock().unwrap().as_ref().and_then(|m| m.get(&key).copied());
        if let Some(m) = m {
            menu = load_menu(c.cpu, lin(m));
        }
    }
    if style & 0x4000_0000 != 0 {
        menu = c.w(o + 8) as u32; // (a child's id)
    }
    let r = call32(c.cpu, "user32!CreateWindowExA", &[exstyle, cls, c.a(o + 1), style, x, y, w, h, c.h(o + 7), menu, 0, c.a(o + 10)]);
    h16(r) as u32
}

fn text_metrics(c: &mut C16) -> u32 {
    let tm = crate::mem::heap_alloc(64, true);
    let r = call32(c.cpu, "gdi32!GetTextMetricsA", &[c.h(0), tm]);
    let p = c.a(1);
    for i in 0..11 {
        w16(p + 2 * i, r32(tm + 4 * i) as u16);
    }
    crate::mem::copy(p + 22, tm + 44, 9);
    crate::mem::heap_free(tm);
    r
}

fn get_object(c: &mut C16) -> u32 {
    let (h, n, p) = (c.h(0), c.s(1), c.a(2));
    let b = crate::mem::heap_alloc(128, true);
    let r = call32(c.cpu, "gdi32!GetObjectA", &[h, 128, b]);
    let r16v = match r {
        // BITMAP
        24 => {
            let mut t = [0u8; 14];
            for i in 0..4 {
                t[2 * i..2 * i + 2].copy_from_slice(&(r32(b + 4 * i as u32) as u16).to_le_bytes());
            }
            t[8] = r16(b + 16) as u8;
            t[9] = r16(b + 18) as u8;
            crate::mem::write(p, &t[..(n.max(0) as usize).min(14)]);
            14
        }
        // LOGFONT
        60 => {
            let mut t = vec![0u8; 50];
            for i in 0..5 {
                t[2 * i..2 * i + 2].copy_from_slice(&(r32(b + 4 * i as u32) as u16).to_le_bytes());
            }
            t[10..50].copy_from_slice(crate::mem::slice(b + 20, 40));
            crate::mem::write(p, &t[..(n.max(0) as usize).min(50)]);
            50
        }
        // LOGPEN
        16 => {
            let mut t = vec![0u8; 10];
            t[0..2].copy_from_slice(&(r32(b) as u16).to_le_bytes());
            t[2..4].copy_from_slice(&(r32(b + 4) as u16).to_le_bytes());
            t[4..6].copy_from_slice(&(r32(b + 8) as u16).to_le_bytes());
            t[6..10].copy_from_slice(&r32(b + 12).to_le_bytes());
            crate::mem::write(p, &t[..(n.max(0) as usize).min(10)]);
            10
        }
        // LOGBRUSH
        12 => {
            let mut t = vec![0u8; 8];
            t[0..2].copy_from_slice(&(r32(b) as u16).to_le_bytes());
            t[2..6].copy_from_slice(&r32(b + 4).to_le_bytes());
            t[6..8].copy_from_slice(&(r32(b + 8) as u16).to_le_bytes());
            crate::mem::write(p, &t[..(n.max(0) as usize).min(8)]);
            8
        }
        // (a palette's entry count)
        _ => {
            crate::mem::write(p, &crate::mem::slice(b, r.min(n.max(0) as u32)).to_vec());
            r
        }
    };
    crate::mem::heap_free(b);
    r16v
}

// ---------------------------------------------------------------------------------------------
// resources

/// A resource of the program: (linear address of its data, size). `ty`/`id` are integer ids
/// (with 0x8000_0000) or linear addresses of names.
pub fn find_resource(ty: u32, id: u32) -> Option<(u32, u32)> {
    let t = task();
    let d = &t.file;
    let ne = t.ne as usize;
    // (the program's resource table, read bounded: 0 past the file's end, a damaged or packed
    // table ends the search instead of aborting the VM)
    let u16at = |o: usize| d.get(o..o.saturating_add(2)).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    let rt = ne + u16at(ne + 0x24) as usize;
    let shift = u16at(rt) as u32;
    if shift > 16 {
        return None;
    }
    let name_at = |o: u16| -> Vec<u8> {
        let p = rt + o as usize;
        let n = d.get(p).copied().unwrap_or(0) as usize;
        d.get(p + 1..).map_or(Vec::new(), |s| s[..n.min(s.len())].to_vec())
    };
    // (an integer id: with 0x8000_0000, or below 0x10000 as MAKEINTRESOURCE and RT_* are)
    let matches = |v: u16, want: u32| -> bool {
        if want & 0x8000_0000 != 0 || want < 0x10000 {
            v == (want as u16) | 0x8000
        } else {
            v & 0x8000 == 0 && name_at(v).eq_ignore_ascii_case(&cstr(want))
        }
    };
    let mut p = rt + 2;
    loop {
        let tid = u16at(p);
        if tid == 0 || p >= d.len() {
            return None;
        }
        let n = u16at(p + 2) as usize;
        p += 8;
        let hit = matches(tid, ty);
        for k in 0..n {
            let e = p + 12 * k;
            let (off, len, rid) = (u16at(e), u16at(e + 2), u16at(e + 6));
            if hit && matches(rid, id) {
                let (o, l) = ((off as usize) << shift, (len as usize) << shift);
                let data = d.get(o.min(d.len())..(o + l).min(d.len())).unwrap_or(&[]);
                let a = crate::mem::heap_alloc(data.len() as u32 + 16, true);
                if a == 0 {
                    return None;
                }
                crate::mem::write(a, data);
                return Some((a, data.len() as u32));
            }
        }
        p += 12 * n;
    }
}

/// A device-dependent bitmap of the packed DIB at `d` (`len` bytes: a BITMAP resource, a
/// BITMAPINFOHEADER or OS/2's BITMAPCOREHEADER, its colours, its bits), as LoadBitmap makes: a
/// monochrome one for two colours black and white, else one like the screen's. 0 if it is
/// not one.
pub fn bitmap_from_dib(cpu: &mut Cpu, d: u32, len: u32) -> u32 {
    let Some(info) = dib_info(crate::mem::slice(d, len)) else { return 0 };
    let bmi = crate::mem::heap_alloc(info.header.len() as u32, true);
    crate::mem::write(bmi, &info.header);
    let bits = d + info.bits;
    let (w, h) = (info.width, info.height);
    let hbm = if info.mono {
        let b = call32(cpu, "gdi32!CreateBitmap", &[w as u32, h.unsigned_abs(), 1, 1, 0]);
        let dc = call32(cpu, "user32!GetDC", &[0]);
        call32(cpu, "gdi32!SetDIBits", &[dc, b, 0, h.unsigned_abs(), bits, bmi, 0]);
        call32(cpu, "user32!ReleaseDC", &[0, dc]);
        b
    } else {
        let dc = call32(cpu, "user32!GetDC", &[0]);
        // CBM_INIT, DIB_RGB_COLORS
        let b = call32(cpu, "gdi32!CreateDIBitmap", &[dc, bmi, 4, bits, bmi, 0]);
        call32(cpu, "user32!ReleaseDC", &[0, dc]);
        b
    };
    crate::mem::heap_free(bmi);
    hbm
}

/// A packed DIB's BITMAPINFO (a core header made an info header, its RGBTRIPLEs RGBQUADs) and
/// where its bits start in it.
#[derive(Debug, PartialEq)]
pub struct DibInfo {
    pub header: Vec<u8>,
    pub bits: u32,
    pub width: i32,
    pub height: i32,
    pub mono: bool,
}

pub fn dib_info(b: &[u8]) -> Option<DibInfo> {
    let u16at = |o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let u32at = |o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    let size = u32at(0)?;
    let (mut header, ncol, entry, width, height, bpp);
    if size == 12 {
        // BITMAPCOREHEADER: WORD width, height, planes, bit count
        width = u16at(4)? as i32;
        height = u16at(6)? as i16 as i32;
        bpp = u16at(10)?;
        ncol = if bpp <= 8 { 1usize << bpp } else { 0 };
        entry = 3;
        header = vec![0u8; 40];
        header[0..4].copy_from_slice(&40u32.to_le_bytes());
        header[4..8].copy_from_slice(&width.to_le_bytes());
        header[8..12].copy_from_slice(&height.to_le_bytes());
        header[12..14].copy_from_slice(&1u16.to_le_bytes());
        header[14..16].copy_from_slice(&bpp.to_le_bytes());
    } else if (40..=124).contains(&size) {
        width = u32at(4)? as i32;
        height = u32at(8)? as i32;
        bpp = u16at(14)?;
        let compression = u32at(16)?;
        let used = u32at(32)? as usize;
        ncol = if used != 0 { used.min(256) } else if bpp <= 8 { 1 << bpp } else { 0 };
        // (BI_BITFIELDS in a 40-byte header: three masks follow it)
        let masks = if size == 40 && compression == 3 { 12 } else { 0 };
        entry = 4;
        header = b.get(..size as usize + masks)?.to_vec();
    } else {
        return None;
    }
    if !(1..=32).contains(&bpp) || width <= 0 || height == 0 {
        return None;
    }
    let start = if size == 12 { 12 } else { header.len() };
    let mut colors = Vec::with_capacity(ncol * 4);
    for k in 0..ncol {
        let e = b.get(start + entry * k..start + entry * k + 3)?;
        colors.extend_from_slice(&[e[0], e[1], e[2], 0]);
    }
    let mono = bpp == 1 && ncol == 2 && colors[0..3] == [0, 0, 0] && colors[4..7] == [255, 255, 255];
    header.extend_from_slice(&colors);
    let bits = (start + entry * ncol) as u32;
    let row = ((width as u64 * bpp as u64 + 31) / 32 * 4) as u32;
    if bits as u64 + row as u64 * height.unsigned_abs() as u64 > b.len() as u64 {
        return None;
    }
    Some(DibInfo { header, bits, width, height, mono })
}

/// A resource name argument (far pointer or MAKEINTRESOURCE) as `find_resource` takes it.
fn res_id(v: u32) -> u32 {
    if v >> 16 == 0 { v | 0x8000_0000 } else { lin(v) }
}

fn load_menu(cpu: &mut Cpu, name: u32) -> u32 {
    let Some((d, _)) = find_resource(4, if name < 0x10000 { name | 0x8000_0000 } else { name }) else { return 0 };
    let m = call32(cpu, "user32!CreateMenu", &[]);
    let mut p = d + 4;
    menu_items(cpu, m, &mut p);
    crate::mem::heap_free(d);
    m
}

fn menu_items(cpu: &mut Cpu, menu: u32, p: &mut u32) {
    menu_items_at(cpu, menu, p, 0)
}

/// (popups nested at most 16 deep and at most 4096 items, as user.rs's MENU_DEPTH bounds the
/// 32-bit side: a damaged resource used to recurse or read on without end)
fn menu_items_at(cpu: &mut Cpu, menu: u32, p: &mut u32, depth: u32) {
    if depth > 16 {
        return;
    }
    for _ in 0..4096 {
        let flags = r16(*p) as u32;
        *p += 2;
        let popup = flags & 0x10 != 0;
        let id = if popup {
            0
        } else {
            let v = r16(*p) as u32;
            *p += 2;
            v
        };
        let text = *p;
        *p += cstr(text).len() as u32 + 1;
        let f = flags & !0x80;
        if popup {
            let sub = call32(cpu, "user32!CreatePopupMenu", &[]);
            menu_items_at(cpu, sub, p, depth + 1);
            call32(cpu, "user32!AppendMenuA", &[menu, f, sub, text]);
        } else {
            let t = if crate::mem::r8(text) == 0 && id == 0 { 0 } else { text };
            call32(cpu, "user32!AppendMenuA", &[menu, if t == 0 { f | 0x800 } else { f }, id, t]);
        }
        if flags & 0x80 != 0 {
            return;
        }
    }
}

/// DLGTEMPLATE16 -> DLGTEMPLATE (a new heap block).
fn dialog32(t: u32) -> u32 {
    let mut out: Vec<u8> = vec![];
    let push16 = |o: &mut Vec<u8>, v: u16| o.extend_from_slice(&v.to_le_bytes());
    let push32 = |o: &mut Vec<u8>, v: u32| o.extend_from_slice(&v.to_le_bytes());
    let wstr = |o: &mut Vec<u8>, a: u32| -> u32 {
        let s = cstr(a);
        for u in crate::text::a2w(&s) {
            o.extend_from_slice(&u.to_le_bytes());
        }
        o.extend_from_slice(&[0, 0]);
        s.len() as u32 + 1
    };
    let mut p = t;
    let style = r32(p);
    let n = crate::mem::r8(p + 4);
    push32(&mut out, style);
    push32(&mut out, 0);
    push16(&mut out, n as u16);
    for i in 0..4 {
        push16(&mut out, r16(p + 5 + 2 * i));
    }
    p += 13;
    // menu, class: 0, 0xff + id, or a string
    for _ in 0..2 {
        match crate::mem::r8(p) {
            0 => {
                push16(&mut out, 0);
                p += 1;
            }
            0xff => {
                push16(&mut out, 0xffff);
                push16(&mut out, r16(p + 1));
                p += 3;
            }
            _ => p += wstr(&mut out, p),
        }
    }
    p += wstr(&mut out, p);
    if style & 0x40 != 0 {
        push16(&mut out, r16(p));
        p += 2;
        p += wstr(&mut out, p);
    }
    for _ in 0..n {
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let (x, y, cx, cy, id, st) = (r16(p), r16(p + 2), r16(p + 4), r16(p + 6), r16(p + 8), r32(p + 10));
        p += 14;
        push32(&mut out, st);
        push32(&mut out, 0);
        for v in [x, y, cx, cy, id] {
            push16(&mut out, v);
        }
        let cls = crate::mem::r8(p);
        if cls & 0x80 != 0 {
            push16(&mut out, 0xffff);
            push16(&mut out, cls as u16);
            p += 1;
        } else {
            p += wstr(&mut out, p);
        }
        if crate::mem::r8(p) == 0xff {
            push16(&mut out, 0xffff);
            push16(&mut out, r16(p + 1));
            p += 3;
        } else {
            p += wstr(&mut out, p);
        }
        let extra = crate::mem::r8(p) as u32;
        push16(&mut out, extra as u16);
        out.extend_from_slice(crate::mem::slice(p + 1, extra));
        p += 1 + extra;
    }
    let b = crate::mem::heap_alloc(out.len() as u32 + 16, true);
    crate::mem::write(b, &out);
    b
}

fn dialog_box(c: &mut C16, param: u32) -> u32 {
    let Some((d, _)) = find_resource(5, res_id(c.args_raw(1))) else {
        eprintln!("sake w16: DialogBox: no template");
        return 0xffff;
    };
    let t = dialog32(d);
    let proc = proc_thunk(c.a(3), Kind::Dlg);
    let r = call32(c.cpu, "user32!DialogBoxIndirectParamA", &[0, t, c.h(2), proc, param]);
    crate::mem::heap_free(t);
    crate::mem::heap_free(d);
    r
}

impl C16<'_> {
    /// An argument's value as pushed (a `p` pointer before conversion).
    pub fn args_raw(&self, i: usize) -> u32 {
        self.raw[i]
    }
}

/// OPENFILENAME16 -> OPENFILENAMEA, GetOpenFileNameA, back.
fn open_file_name(c: &mut C16) -> u32 {
    let p = lin(c.a(0));
    let o = crate::mem::heap_alloc(0x58, true);
    w32(o, 0x4c);
    w32(o + 4, h32(r16(p + 4)));
    w32(o + 12, lin(r32(p + 8)));
    w32(o + 16, lin(r32(p + 12)));
    w32(o + 20, r32(p + 16));
    w32(o + 24, r32(p + 20));
    w32(o + 28, lin(r32(p + 24)));
    w32(o + 32, r32(p + 28));
    w32(o + 36, lin(r32(p + 32)));
    w32(o + 40, r32(p + 36));
    w32(o + 44, lin(r32(p + 40)));
    w32(o + 48, lin(r32(p + 44)));
    w32(o + 52, r32(p + 48) & !0x20); // (no hook)
    let r = call32(c.cpu, "comdlg32!GetOpenFileNameA", &[o]);
    w16(p + 52, r16(o + 56));
    w16(p + 54, r16(o + 58));
    crate::mem::heap_free(o);
    r
}

// ---------------------------------------------------------------------------------------------
// wsprintf

/// wsprintf's format with 16-bit arguments at `va` (linear).
fn format16(fmt: u32, va: u32) -> Vec<u8> {
    let f = cstr(fmt);
    let mut out = vec![];
    let mut a = va;
    let mut i = 0;
    while i < f.len() {
        let ch = f[i];
        if ch != b'%' {
            out.push(ch);
            i += 1;
            continue;
        }
        i += 1;
        let mut left = false;
        let mut zero = false;
        while i < f.len() && matches!(f[i], b'-' | b'0' | b'#') {
            if f[i] == b'-' {
                left = true;
            }
            if f[i] == b'0' {
                zero = true;
            }
            i += 1;
        }
        let mut width = 0usize;
        while i < f.len() && f[i].is_ascii_digit() {
            width = width * 10 + (f[i] - b'0') as usize;
            i += 1;
        }
        let mut prec = None;
        if i < f.len() && f[i] == b'.' {
            i += 1;
            let mut v = 0;
            while i < f.len() && f[i].is_ascii_digit() {
                v = v * 10 + (f[i] - b'0') as usize;
                i += 1;
            }
            prec = Some(v);
        }
        let mut long = false;
        if i < f.len() && (f[i] == b'l' || f[i] == b'L') {
            long = true;
            i += 1;
        }
        if i >= f.len() {
            break;
        }
        let conv = f[i];
        i += 1;
        let mut take = |long: bool| -> u32 {
            let v = if long { r32(a) } else { r16(a) as u32 };
            a += if long { 4 } else { 2 };
            v
        };
        let s: Vec<u8> = match conv {
            b'd' | b'i' => {
                let v = take(long);
                let v = if long { v as i32 as i64 } else { v as u16 as i16 as i64 };
                v.to_string().into_bytes()
            }
            b'u' => take(long).to_string().into_bytes(),
            b'x' => format!("{:x}", take(long)).into_bytes(),
            b'X' => format!("{:X}", take(long)).into_bytes(),
            b'c' => vec![take(false) as u8],
            b's' => {
                let p = lin(take(true));
                let mut s = if p == 0 { b"(null)".to_vec() } else { cstr(p) };
                if let Some(n) = prec {
                    s.truncate(n);
                }
                s
            }
            b'%' => vec![b'%'],
            other => vec![b'%', other],
        };
        let pad = width.saturating_sub(s.len());
        if left {
            out.extend_from_slice(&s);
            out.extend(std::iter::repeat_n(b' ', pad));
        } else {
            let c = if zero && conv != b's' { b'0' } else { b' ' };
            out.extend(std::iter::repeat_n(c, pad));
            out.extend_from_slice(&s);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// DOS (INT 21h, DOS3Call)

struct Dos {
    dta: u32,
    finds: HashMap<u32, u32>,
}

static DOS: Mutex<Option<Dos>> = Mutex::new(None);

fn ds_dx(cpu: &Cpu) -> u32 {
    cpu.sbase[3].wrapping_add(cpu.regs[EDX] & 0xffff)
}

/// INT 21h with the registers in `cpu` (carry set on error, AX the error code).
pub fn dos(cpu: &mut Cpu) {
    let ax = cpu.regs[EAX] & 0xffff;
    let ah = ax >> 8;
    let set_ax = |cpu: &mut Cpu, v: u32| cpu.regs[EAX] = (cpu.regs[EAX] & 0xffff_0000) | (v & 0xffff);
    let ok = |cpu: &mut Cpu| cpu.eflags &= !1;
    let err = |cpu: &mut Cpu, e: u32| {
        cpu.eflags |= 1;
        cpu.regs[EAX] = (cpu.regs[EAX] & 0xffff_0000) | e;
    };
    cpu.materialize();
    match ah {
        0x30 => set_ax(cpu, 0x0005),
        0x19 => set_ax(cpu, (ax & 0xff00) | 2), // C:
        0x0e => set_ax(cpu, (ax & 0xff00) | 26),
        0x2a => {
            let (y, mo, d, wd) = date(cpu);
            cpu.regs[ECX] = (cpu.regs[ECX] & 0xffff_0000) | y;
            cpu.regs[EDX] = (cpu.regs[EDX] & 0xffff_0000) | mo << 8 | d;
            set_ax(cpu, (ax & 0xff00) | wd);
        }
        0x2c => {
            let ms = crate::k32::ticks_ms() as u64;
            let s = (ms / 1000) % 86400;
            cpu.regs[ECX] = (cpu.regs[ECX] & 0xffff_0000) | ((s / 3600) as u32) << 8 | ((s / 60) % 60) as u32;
            cpu.regs[EDX] = (cpu.regs[EDX] & 0xffff_0000) | ((s % 60) as u32) << 8 | ((ms / 10) % 100) as u32;
        }
        0x25 | 0x35 => {
            if ah == 0x35 {
                cpu.regs[3] &= 0xffff_0000;
                w16::set_seg(cpu, 0, 0);
            }
        }
        0x1a => {
            DOS.lock().unwrap().get_or_insert_with(|| Dos { dta: 0, finds: HashMap::default() }).dta = ds_dx(cpu);
        }
        0x3d | 0x3c => {
            let name = ds_dx(cpu);
            let r = if ah == 0x3c {
                crate::w16::call32(cpu, "kernel32!_lcreat", &[name, cpu.regs[ECX] & 0xffff])
            } else {
                crate::w16::call32(cpu, "kernel32!_lopen", &[name, ax & 0xff])
            };
            if r == u32::MAX || r == 0 {
                err(cpu, 2);
            } else {
                ok(cpu);
                set_ax(cpu, r);
            }
        }
        0x3e => {
            crate::w16::call32(cpu, "kernel32!CloseHandle", &[cpu.regs[3] & 0xffff]);
            ok(cpu);
        }
        0x40 if std_write(cpu.regs[3] & 0xffff, ds_dx(cpu), cpu.regs[ECX] & 0xffff).is_some() => {
            ok(cpu);
            set_ax(cpu, cpu.regs[ECX] & 0xffff);
        }
        0x3f | 0x40 => {
            let h = cpu.regs[3] & 0xffff;
            let (buf, n) = (ds_dx(cpu), cpu.regs[ECX] & 0xffff);
            let got = crate::mem::heap_alloc(4, true);
            let f = if ah == 0x3f { "kernel32!ReadFile" } else { "kernel32!WriteFile" };
            let r = crate::w16::call32(cpu, f, &[h, buf, n, got, 0]);
            let g = r32(got);
            crate::mem::heap_free(got);
            if r == 0 {
                err(cpu, 5);
            } else {
                ok(cpu);
                set_ax(cpu, g);
            }
        }
        0x42 => {
            let h = cpu.regs[3] & 0xffff;
            let off = (cpu.regs[ECX] & 0xffff) << 16 | (cpu.regs[EDX] & 0xffff);
            let r = crate::w16::call32(cpu, "kernel32!SetFilePointer", &[h, off, 0, ax & 0xff]);
            if r == u32::MAX {
                err(cpu, 6);
            } else {
                ok(cpu);
                set_ax(cpu, r);
                cpu.regs[EDX] = (cpu.regs[EDX] & 0xffff_0000) | r >> 16;
            }
        }
        0x44 => {
            // IOCTL: get device info -> a disk file
            cpu.regs[EDX] = cpu.regs[EDX] & 0xffff_0000 | if cpu.regs[3] & 0xffff < 3 { 0x80d3 } else { 0x0002 };
            ok(cpu);
        }
        0x47 => {
            let cwd = crate::files::cwd();
            let s = crate::text::s2a(cwd.trim_end_matches('\\'));
            let s = if s.len() > 3 { s[3..].to_vec() } else { vec![] };
            let d = cpu.sbase[3].wrapping_add(cpu.regs[6] & 0xffff);
            crate::mem::write(d, &s);
            w8(d + s.len() as u32, 0);
            ok(cpu);
        }
        0x3b => {
            let p = crate::text::a2s(&cstr(ds_dx(cpu)));
            crate::files::set_cwd(&format!("{}\\", p.trim_end_matches('\\')));
            ok(cpu);
        }
        0x43 => {
            let r = crate::w16::call32(cpu, "kernel32!GetFileAttributesA", &[ds_dx(cpu)]);
            if r == u32::MAX {
                err(cpu, 2);
            } else {
                cpu.regs[ECX] = cpu.regs[ECX] & 0xffff_0000 | (r & 0x3f);
                ok(cpu);
            }
        }
        0x41 => {
            let r = crate::w16::call32(cpu, "kernel32!DeleteFileA", &[ds_dx(cpu)]);
            if r == 0 { err(cpu, 2) } else { ok(cpu) }
        }
        0x4c => {
            unsafe { libc::exit((ax & 0xff) as i32) }
        }
        0x4e | 0x4f => {
            let mut g = DOS.lock().unwrap();
            let dos = g.get_or_insert_with(|| Dos { dta: 0, finds: HashMap::default() });
            let dta = if dos.dta == 0 { w16::sel_base(task().psp).unwrap().0 + 0x80 } else { dos.dta };
            let fd = crate::mem::heap_alloc(320, true);
            let (h, r) = if ah == 0x4e {
                // (a new search at this DTA ends the one before: its find handle is closed)
                let old = dos.finds.remove(&dta);
                drop(g);
                if let Some(o) = old {
                    crate::w16::call32(cpu, "kernel32!FindClose", &[o]);
                }
                let h = crate::w16::call32(cpu, "kernel32!FindFirstFileA", &[ds_dx(cpu), fd]);
                (h, h != u32::MAX)
            } else {
                let h = dos.finds.get(&dta).copied().unwrap_or(u32::MAX);
                drop(g);
                (h, h != u32::MAX && crate::w16::call32(cpu, "kernel32!FindNextFileA", &[h, fd]) != 0)
            };
            // (no more files: the search is over and its handle closed, as DOS has none to keep)
            if !r && h != u32::MAX {
                DOS.lock().unwrap().as_mut().unwrap().finds.remove(&dta);
                crate::w16::call32(cpu, "kernel32!FindClose", &[h]);
            }
            if r {
                DOS.lock().unwrap().as_mut().unwrap().finds.insert(dta, h);
                w8(dta + 0x15, r32(fd) as u8);
                w32(dta + 0x1a, r32(fd + 32));
                let name = cstr(fd + 44);
                crate::mem::put_cstr(dta + 0x1e, 13, &name);
                ok(cpu);
            } else {
                err(cpu, 0x12);
            }
            crate::mem::heap_free(fd);
        }
        0x2f => {
            let dta = DOS.lock().unwrap().as_ref().map_or(0, |d| d.dta);
            let _ = dta;
            ok(cpu);
        }
        0x48 => {
            let paras = cpu.regs[3] & 0xffff;
            let s = w16::global_alloc(paras * 16, true);
            if s == 0 { err(cpu, 8) } else { ok(cpu); set_ax(cpu, s as u32) }
        }
        0x49 => {
            w16::global_free(cpu.sreg[0]);
            ok(cpu);
        }
        0x0b => set_ax(cpu, ax & 0xff00),
        0x33 => {
            cpu.regs[EDX] &= 0xffff_ff00;
        }
        _ => {
            eprintln!("sake w16: DOS function {ah:#x} (ax {ax:04x})");
            err(cpu, 1);
        }
    }
}

/// Other interrupts: nothing there.
pub fn int_other(cpu: &mut Cpu, n: u32) {
    let ax = cpu.regs[EAX] & 0xffff;
    if n == 0x2f && ax == 0x1600 {
        // (no enhanced mode Windows check)
        cpu.regs[EAX] = cpu.regs[EAX] & 0xffff_0000 | 0x0a03;
    }
    let _ = r32;
}

/// (year, month, day, weekday) of today (local time).
pub fn date(cpu: &mut Cpu) -> (u32, u32, u32, u32) {
    let st = crate::mem::heap_alloc(16, true);
    call32(cpu, "kernel32!GetLocalTime", &[st]);
    let r = (r16(st) as u32, r16(st + 2) as u32, r16(st + 6) as u32, r16(st + 4) as u32);
    crate::mem::heap_free(st);
    r
}

#[cfg(test)]
mod dib_tests {
    use super::*;
    use proptest::prelude::*;

    fn info_dib(w: i32, h: i32, bpp: u16, colors: &[[u8; 4]], used: u32) -> Vec<u8> {
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&40u32.to_le_bytes());
        b[4..8].copy_from_slice(&w.to_le_bytes());
        b[8..12].copy_from_slice(&h.to_le_bytes());
        b[12..14].copy_from_slice(&1u16.to_le_bytes());
        b[14..16].copy_from_slice(&bpp.to_le_bytes());
        b[32..36].copy_from_slice(&used.to_le_bytes());
        for c in colors {
            b.extend_from_slice(c);
        }
        let row = ((w as usize * bpp as usize + 31) / 32) * 4;
        b.extend(std::iter::repeat(0x5a).take(row * h.unsigned_abs() as usize));
        b
    }

    #[test]
    fn headers_colours_and_bits() {
        // 1 bpp black and white: monochrome; bits after the two colours
        let d = info_dib(8, 2, 1, &[[0, 0, 0, 0], [255, 255, 255, 0]], 0);
        let i = dib_info(&d).unwrap();
        assert_eq!((i.bits, i.mono, i.header.len()), (48, true, 48));
        // two other colours: not monochrome
        assert!(!dib_info(&info_dib(8, 2, 1, &[[0, 0, 255, 0], [255, 255, 255, 0]], 0)).unwrap().mono);
        // 4 bpp with biClrUsed 3
        let d = info_dib(3, -2, 4, &[[1, 2, 3, 0], [4, 5, 6, 0], [7, 8, 9, 0]], 3);
        let i = dib_info(&d).unwrap();
        assert_eq!((i.bits, i.height), (52, -2));
        // 24 bpp: no colours
        assert_eq!(dib_info(&info_dib(5, 5, 24, &[], 0)).unwrap().bits, 40);
        // a core header: RGBTRIPLEs made RGBQUADs, an info header
        let mut c = vec![12, 0, 0, 0, 2, 0, 1, 0, 1, 0, 8, 0];
        for k in 0..256u32 {
            c.extend_from_slice(&[k as u8, 1, 2]);
        }
        c.extend_from_slice(&[0; 4]);
        let i = dib_info(&c).unwrap();
        assert_eq!((i.bits, i.header.len(), i.width, i.height), (12 + 768, 40 + 1024, 2, 1));
        assert_eq!(&i.header[40 + 4 * 7..40 + 4 * 8], &[7, 1, 2, 0]);
        // too short, a bad size, no width
        assert!(dib_info(&d[..d.len() - 1]).is_none());
        assert!(dib_info(&[20, 0, 0, 0]).is_none());
        assert!(dib_info(&info_dib(0, 1, 8, &[[0; 4]; 256], 0)).is_none());
    }

    proptest! {
        /// Any well-formed info DIB: the bits start after header and colours, the header keeps
        /// the colours, and one byte less of bits is refused
        #[test]
        fn bits_follow_the_colours(w in 1i32..64, h in -16i32..16, bpp in proptest::sample::select(vec![1u16, 4, 8, 16, 24, 32]), used in 0u32..4) {
            prop_assume!(h != 0);
            let n = if bpp <= 8 { if used != 0 { used as usize } else { 1 << bpp } } else { used as usize };
            let colors: Vec<[u8; 4]> = (0..n).map(|k| [k as u8, 2, 3, 0]).collect();
            let d = info_dib(w, h, bpp, &colors, used);
            let i = dib_info(&d).unwrap();
            prop_assert_eq!(i.bits as usize, 40 + 4 * n);
            prop_assert_eq!(&i.header[40..], &d[40..40 + 4 * n]);
            prop_assert!(dib_info(&d[..d.len() - 1]).is_none());
        }
    }
}
