#![allow(non_snake_case)]
//! cfgmgr32.dll (the Configuration Manager: device interfaces, device nodes, PnP notifications)
//! and user32's RegisterDeviceNotification, for a machine whose device tree is the root node
//! alone: no device interface of any class, so device enumerations answer an empty list.
//!
//! Windows always has cfgmgr32.dll. Without it LoadLibrary fails and SDL3's HIDAPI (Godot 4.6's
//! joypads, #104) retries the load every frame, a failing DLL search 20-60 times a second for the
//! whole run. With it, hid_init succeeds, CM_Get_Device_Interface_List(GUID_DEVINTERFACE_HID)
//! answers no devices, and RegisterDeviceNotification gives the handle that tells SDL it will be
//! told about new devices (WM_DEVICECHANGE, which never comes here).
//!
//! The answers follow Wine's cfgmgr32 tests (Windows' results): argument checks in Windows'
//! order, CR_FAILURE for a NULL class, an empty list = one NUL (size 1), an instance id that
//! names no device node = CR_INVALID_DEVNODE (lists) or CR_NO_SUCH_DEVNODE (CM_Locate_DevNode),
//! one without a backslash = CR_INVALID_DEVICE_ID, NULL / "" / "HTREE\ROOT\0" = the root node.
//! hid.dll's HidD_GetHidGuid is here too (the class those lists are asked for).

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem;

pub const CR_SUCCESS: u32 = 0x00;
pub const CR_OUT_OF_MEMORY: u32 = 0x02;
pub const CR_INVALID_POINTER: u32 = 0x03;
pub const CR_INVALID_FLAG: u32 = 0x04;
pub const CR_INVALID_DEVNODE: u32 = 0x05;
pub const CR_NO_SUCH_DEVNODE: u32 = 0x0d;
pub const CR_FAILURE: u32 = 0x13;
pub const CR_BUFFER_SMALL: u32 = 0x1a;
pub const CR_INVALID_DEVICE_ID: u32 = 0x1e;
pub const CR_NO_SUCH_VALUE: u32 = 0x25;
pub const CR_INVALID_DATA: u32 = 0x1f;
pub const CR_NO_SUCH_DEVICE_INTERFACE: u32 = 0x37;

/// CM_GET_DEVICE_INTERFACE_LIST_PRESENT (0) / _ALL_DEVICES (1): the only flags
const LIST_FLAGS: u32 = 1;
/// The root device node's DEVINST (Windows numbers nodes from 1 as they are located)
pub const ROOT: u32 = 1;
/// DN_DRIVER_LOADED | DN_STARTED | DN_NT_ENUMERATOR | DN_NT_DRIVER: the root node's status
pub const ROOT_STATUS: u32 = 0x0000_0002 | 0x0000_0008 | 0x0080_0000 | 0x0100_0000;
/// sizeof(CM_NOTIFY_FILTER): cbSize, Flags, FilterType, Reserved, a union of 400 bytes
pub const NOTIFY_FILTER_SIZE: u32 = 416;

/// How an instance id names a node: none given (NULL or ""), the root, a well-formed id of no
/// node here, or a malformed one (no backslash)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instance {
    None,
    Root,
    Unknown,
    Malformed,
}

pub fn classify(id: Option<&[u16]>) -> Instance {
    let Some(id) = id.filter(|s| !s.is_empty()) else { return Instance::None };
    let s = String::from_utf16_lossy(id);
    if s.eq_ignore_ascii_case("HTREE\\ROOT\\0") {
        Instance::Root
    } else if s.contains('\\') {
        Instance::Unknown
    } else {
        Instance::Malformed
    }
}

/// CM_Get_Device_Interface_List_Size: the result and the length (characters) to store, if any.
/// (There are no interfaces: an empty list, its one NUL; for a device given by id, that device
/// has to exist, and none but the root does, which has no interfaces either.)
pub fn list_size(len: bool, class: bool, instance: Instance, flags: u32) -> (u32, Option<u32>) {
    if !class {
        return (CR_FAILURE, None);
    }
    if !len {
        return (CR_INVALID_POINTER, None);
    }
    if flags & !LIST_FLAGS != 0 {
        return (CR_INVALID_FLAG, None);
    }
    match instance {
        Instance::None | Instance::Root => (CR_SUCCESS, Some(1)),
        Instance::Unknown | Instance::Malformed => (CR_INVALID_DEVNODE, Some(0)),
    }
}

/// CM_Get_Device_Interface_List: the result and whether the buffer (`len` characters) is
/// cleared (Windows zero-fills it before it appends the interfaces and the final NUL).
pub fn list(class: bool, buffer: bool, len: u32, instance: Instance, flags: u32) -> (u32, bool) {
    if !class {
        return (CR_FAILURE, false);
    }
    if !buffer {
        return (CR_INVALID_POINTER, false);
    }
    if len == 0 {
        return (CR_BUFFER_SMALL, false);
    }
    if flags & !LIST_FLAGS != 0 {
        return (CR_INVALID_FLAG, false);
    }
    match instance {
        Instance::None | Instance::Root => (CR_SUCCESS, true),
        Instance::Unknown | Instance::Malformed => (CR_INVALID_DEVNODE, false),
    }
}

/// CM_Locate_DevNode: the result and the DEVINST stored (0 on failure, as Windows clears it)
pub fn locate(instance: Instance) -> (u32, u32) {
    match instance {
        Instance::None | Instance::Root => (CR_SUCCESS, ROOT),
        Instance::Unknown => (CR_NO_SUCH_DEVNODE, 0),
        Instance::Malformed => (CR_INVALID_DEVICE_ID, 0),
    }
}

/// CM_MapCrToWin32Err(code, default)
pub fn cr_to_win32(cr: u32, default: u32) -> u32 {
    match cr {
        CR_SUCCESS => 0,
        CR_OUT_OF_MEMORY => 8,                // ERROR_NOT_ENOUGH_MEMORY
        CR_INVALID_POINTER => 1784,           // ERROR_INVALID_USER_BUFFER
        CR_INVALID_FLAG => 1004,              // ERROR_INVALID_FLAGS
        CR_INVALID_DEVNODE | CR_INVALID_DEVICE_ID | 0x2f /* MACHINENAME */ | 0x35 /* PROPERTY */ | 0x38 /* REFERENCE_STRING */ => 13, // ERROR_INVALID_DATA
        CR_NO_SUCH_DEVNODE | CR_NO_SUCH_VALUE | CR_NO_SUCH_DEVICE_INTERFACE => 1168, // ERROR_NOT_FOUND
        0x10 => 183,                          // CR_ALREADY_SUCH_DEVNODE: ERROR_ALREADY_EXISTS
        CR_BUFFER_SMALL => 122,               // ERROR_INSUFFICIENT_BUFFER
        0x1c => 6,                            // CR_NO_REGISTRY_HANDLE: ERROR_INVALID_HANDLE
        0x1d => 1015,                         // CR_REGISTRY_ERROR: ERROR_REGISTRY_CORRUPT
        0x2e => 2,                            // CR_NO_SUCH_REGISTRY_KEY: ERROR_FILE_NOT_FOUND
        0x30..=0x32 => 1062,                  // REMOTE_COMM_FAILURE, MACHINE_UNAVAILABLE, NO_CM_SERVICES: ERROR_SERVICE_NOT_ACTIVE
        0x33 => 5,                            // CR_ACCESS_DENIED: ERROR_ACCESS_DENIED
        0x34 => 120,                          // CR_CALL_NOT_IMPLEMENTED: ERROR_CALL_NOT_IMPLEMENTED
        _ => default,
    }
}

fn wide_arg(p: u32) -> Option<Vec<u16>> {
    (p != 0).then(|| mem::wstr(p))
}
fn ansi_arg(p: u32) -> Option<Vec<u16>> {
    (p != 0).then(|| crate::text::a2w(&mem::cstr(p)))
}

/// CM_Get_Device_Interface_List_Size[_Ex]{A,W}(len*, class*, instance, flags[, machine])
fn size_of(c: &mut Ctx, wide: bool) -> u32 {
    let (len, class, id, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let inst = classify(if wide { wide_arg(id) } else { ansi_arg(id) }.as_deref());
    let (cr, n) = list_size(len != 0, class != 0, inst, flags);
    if let Some(n) = n {
        mem::w32(len, n);
    }
    cr
}

/// CM_Get_Device_Interface_List[_Ex]{A,W}(class*, instance, buffer, len, flags[, machine])
fn list_of(c: &mut Ctx, wide: bool) -> u32 {
    let (class, id, buf, len, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let inst = classify(if wide { wide_arg(id) } else { ansi_arg(id) }.as_deref());
    let (cr, clear) = list(class != 0, buf != 0, len, inst, flags);
    if clear {
        mem::fill(buf, len.saturating_mul(if wide { 2 } else { 1 }), 0);
    }
    cr
}

/// CM_Locate_DevNode[_Ex]{A,W}(DEVINST*, instance, flags[, machine])
fn locate_of(c: &mut Ctx, wide: bool) -> u32 {
    let (node, id) = (c.a(0), c.a(1));
    if node == 0 {
        return CR_INVALID_POINTER;
    }
    let (cr, n) = locate(classify(if wide { wide_arg(id) } else { ansi_arg(id) }.as_deref()));
    mem::w32(node, n);
    cr
}

/// CM_Get_Parent[_Ex](DEVINST*, node, flags[, machine]): the root has none, others aren't nodes
fn parent(c: &mut Ctx) -> u32 {
    let (out, node) = (c.a(0), c.a(1));
    if out == 0 {
        return CR_INVALID_POINTER;
    }
    mem::w32(out, 0);
    if node == ROOT { CR_NO_SUCH_DEVNODE } else { CR_INVALID_DEVNODE }
}

/// CM_Get_DevNode_Status[_Ex](status*, problem*, node, flags[, machine])
fn status(c: &mut Ctx) -> u32 {
    let (st, pr, node) = (c.a(0), c.a(1), c.a(2));
    if st == 0 || pr == 0 {
        return CR_INVALID_POINTER;
    }
    if node != ROOT {
        return CR_INVALID_DEVNODE;
    }
    mem::w32(st, ROOT_STATUS);
    mem::w32(pr, 0);
    CR_SUCCESS
}

/// The checks of a property query's (key, type*, buffer, size*) (Wine's init_property: a NULL
/// key is CR_FAILURE, NULL type or size, or no buffer for a non-zero size, CR_INVALID_POINTER)
fn property_args(key: u32, ty: u32, buf: u32, size: u32) -> Option<u32> {
    if key == 0 {
        return Some(CR_FAILURE);
    }
    if ty == 0 || size == 0 || (buf == 0 && mem::r32(size) != 0) {
        return Some(CR_INVALID_POINTER);
    }
    None
}

/// CM_Get_DevNode_Property[_Ex]W(node, key, type*, buffer, size*, flags[, machine]): the root
/// node has none of the properties asked for here
fn node_property(c: &mut Ctx) -> u32 {
    if c.a(0) != ROOT {
        return CR_INVALID_DEVNODE;
    }
    property_args(c.a(1), c.a(2), c.a(3), c.a(4)).unwrap_or(CR_NO_SUCH_VALUE)
}

/// CM_Get_Device_Interface_Property[_Ex]W(name, key, type*, buffer, size*, flags[, machine]):
/// no interface exists
fn iface_property(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return CR_INVALID_POINTER;
    }
    CR_NO_SUCH_DEVICE_INTERFACE
}

/// CM_Register_Notification(filter*, context, callback, HCMNOTIFICATION*)
fn register(c: &mut Ctx) -> u32 {
    let (filter, callback, out) = (c.a(0), c.a(2), c.a(3));
    if out == 0 {
        return CR_FAILURE;
    }
    if filter == 0 || callback == 0 || mem::r32(filter) != NOTIFY_FILTER_SIZE {
        return CR_INVALID_DATA;
    }
    let h = mem::heap_alloc(16, true);
    if h == 0 {
        return CR_OUT_OF_MEMORY;
    }
    mem::w32(h, NOTIFY_MAGIC);
    mem::putp(out, h);
    CR_SUCCESS
}
const NOTIFY_MAGIC: u32 = u32::from_le_bytes(*b"CMNf");
const DEVNOTIFY_MAGIC: u32 = u32::from_le_bytes(*b"DvNf");

fn is_handle(h: u32, magic: u32) -> bool {
    h != 0 && mem::heap_owns(h) && mem::r32(h) == magic
}

/// CM_Unregister_Notification(HCMNOTIFICATION)
fn unregister(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if !is_handle(h, NOTIFY_MAGIC) {
        return CR_INVALID_DATA;
    }
    mem::w32(h, 0);
    mem::heap_free(h);
    CR_SUCCESS
}

/// DEVICE_NOTIFY_SERVICE_HANDLE | DEVICE_NOTIFY_ALL_INTERFACE_CLASSES
const DEVNOTIFY_FLAGS: u32 = 1 | 4;
/// DBT_DEVTYP_DEVICEINTERFACE, DBT_DEVTYP_HANDLE: the filters Windows takes
const DBT_DEVTYP_DEVICEINTERFACE: u32 = 5;
const DBT_DEVTYP_HANDLE: u32 = 6;

/// What RegisterDeviceNotification answers: Ok (a handle) or the last error
pub fn devnotify_check(flags: u32, window_ok: bool, filter_type: Option<u32>) -> Result<(), u32> {
    if flags & !DEVNOTIFY_FLAGS != 0 {
        return Err(87); // ERROR_INVALID_PARAMETER
    }
    if flags & 1 == 0 && !window_ok {
        return Err(87);
    }
    match filter_type {
        None | Some(DBT_DEVTYP_DEVICEINTERFACE) | Some(DBT_DEVTYP_HANDLE) => Ok(()),
        Some(_) => Err(13), // ERROR_INVALID_DATA
    }
}

/// RegisterDeviceNotification{A,W}(recipient, filter, flags): a handle; no device ever arrives
fn register_devnotify(c: &mut Ctx) -> u32 {
    let (hwnd, filter, flags) = (c.a(0), c.a(1), c.a(2));
    let ty = (filter != 0).then(|| mem::r32(filter + 4));
    if let Err(e) = devnotify_check(flags, crate::user::is_window(hwnd), ty) {
        last_error(e);
        return 0;
    }
    let h = mem::heap_alloc(16, true);
    if h == 0 {
        last_error(8);
        return 0;
    }
    mem::w32(h, DEVNOTIFY_MAGIC);
    h
}

/// UnregisterDeviceNotification(handle)
fn unregister_devnotify(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if !is_handle(h, DEVNOTIFY_MAGIC) {
        last_error(6); // ERROR_INVALID_HANDLE
        return 0;
    }
    mem::w32(h, 0);
    mem::heap_free(h);
    1
}

/// GUID_DEVINTERFACE_HID {4d1e55b2-f16f-11cf-88cb-001111000030}
pub const GUID_DEVINTERFACE_HID: [u8; 16] = [0xb2, 0x55, 0x1e, 0x4d, 0x6f, 0xf1, 0xcf, 0x11, 0x88, 0xcb, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30];

/// hid.dll's HidD_GetHidGuid(GUID*): the class SDL's HIDAPI lists with
/// CM_Get_Device_Interface_List (void; Windows doesn't check the pointer)
fn hid_guid(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::write(c.a(0), &GUID_DEVINTERFACE_HID);
    }
    0
}

// setupapi's device information sets (SetupDi*): the same machine, no device of any class,
// so a set is always empty. Flash's player (悪魔の石板, #209) lists one at start.

const DEVINFO_MAGIC: u32 = u32::from_le_bytes(*b"DvIS");
/// DIGCF_ALLCLASSES: no class GUID needed
const DIGCF_ALLCLASSES: u32 = 4;

/// What SetupDiGetClassDevs answers for its class GUID (NULL or not) and flags: Ok (a set) or
/// the last error (Wine's setupapi tests: a NULL class without DIGCF_ALLCLASSES is refused)
pub fn class_devs_check(class_null: bool, flags: u32) -> Result<(), u32> {
    if class_null && flags & DIGCF_ALLCLASSES == 0 {
        return Err(87); // ERROR_INVALID_PARAMETER
    }
    Ok(())
}

/// What SetupDiEnumDeviceInfo answers, in Windows' order of checks: the set, the SP_DEVINFO_DATA
/// pointer, its cbSize (28 bytes, 32 on x64), then the index (always past the end here)
pub fn enum_devinfo_error(set_ok: bool, data: Option<u32>, x64: bool) -> u32 {
    if !set_ok {
        return 6; // ERROR_INVALID_HANDLE
    }
    let Some(size) = data else { return 87 };
    if size != if x64 { 32 } else { 28 } {
        return 0x6f8; // ERROR_INVALID_USER_BUFFER
    }
    259 // ERROR_NO_MORE_ITEMS
}

/// SetupDiGetClassDevs{A,W}(class, enumerator, parent, flags) and the Ex forms (.., set,
/// machine, reserved): a new empty set
fn class_devs(c: &mut Ctx) -> u32 {
    if let Err(e) = class_devs_check(c.a(0) == 0, c.a(3)) {
        last_error(e);
        return u32::MAX;
    }
    let h = mem::heap_alloc(16, true);
    if h == 0 {
        last_error(8);
        return u32::MAX;
    }
    mem::w32(h, DEVINFO_MAGIC);
    h
}

fn is_devinfo(h: u32) -> bool {
    h != u32::MAX && is_handle(h, DEVINFO_MAGIC)
}

/// SetupDiEnumDeviceInfo(set, index, SP_DEVINFO_DATA*)
fn enum_devinfo(c: &mut Ctx) -> u32 {
    let (h, data) = (c.a(0), c.a(2));
    last_error(enum_devinfo_error(is_devinfo(h), (data != 0).then(|| mem::r32(data)), mem::x64()));
    0
}

/// SetupDiDestroyDeviceInfoList(set)
fn destroy_devinfo(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if !is_devinfo(h) {
        last_error(6);
        return 0;
    }
    mem::w32(h, 0);
    mem::heap_free(h);
    1
}

pub fn init() {
    implement("hid", "HidD_GetHidGuid", hid_guid);
    for n in ["SetupDiGetClassDevsA", "SetupDiGetClassDevsW", "SetupDiGetClassDevsExA", "SetupDiGetClassDevsExW"] {
        implement("setupapi", n, class_devs);
    }
    implement("setupapi", "SetupDiEnumDeviceInfo", enum_devinfo);
    implement("setupapi", "SetupDiDestroyDeviceInfoList", destroy_devinfo);
    let d = "cfgmgr32";
    for n in ["CM_Get_Device_Interface_List_SizeW", "CM_Get_Device_Interface_List_Size_ExW"] {
        implement(d, n, |c| size_of(c, true));
    }
    for n in ["CM_Get_Device_Interface_List_SizeA", "CM_Get_Device_Interface_List_Size_ExA"] {
        implement(d, n, |c| size_of(c, false));
    }
    for n in ["CM_Get_Device_Interface_ListW", "CM_Get_Device_Interface_List_ExW"] {
        implement(d, n, |c| list_of(c, true));
    }
    for n in ["CM_Get_Device_Interface_ListA", "CM_Get_Device_Interface_List_ExA"] {
        implement(d, n, |c| list_of(c, false));
    }
    for n in ["CM_Locate_DevNodeW", "CM_Locate_DevNode_ExW"] {
        implement(d, n, |c| locate_of(c, true));
    }
    for n in ["CM_Locate_DevNodeA", "CM_Locate_DevNode_ExA"] {
        implement(d, n, |c| locate_of(c, false));
    }
    for n in ["CM_Get_Parent", "CM_Get_Parent_Ex"] {
        implement(d, n, parent);
    }
    for n in ["CM_Get_DevNode_Status", "CM_Get_DevNode_Status_Ex"] {
        implement(d, n, status);
    }
    for n in ["CM_Get_DevNode_PropertyW", "CM_Get_DevNode_Property_ExW"] {
        implement(d, n, node_property);
    }
    for n in ["CM_Get_Device_Interface_PropertyW", "CM_Get_Device_Interface_Property_ExW"] {
        implement(d, n, iface_property);
    }
    implement(d, "CM_Register_Notification", register);
    implement(d, "CM_Unregister_Notification", unregister);
    implement(d, "CM_MapCrToWin32Err", |c| cr_to_win32(c.a(0), c.a(1)));
    for n in ["RegisterDeviceNotificationA", "RegisterDeviceNotificationW"] {
        implement("user32", n, register_devnotify);
    }
    implement("user32", "UnregisterDeviceNotification", unregister_devnotify);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// SetupDi's argument checks as Wine's setupapi tests have Windows answer them
    #[test]
    fn devinfo_answers() {
        assert_eq!(class_devs_check(true, 0), Err(87));
        assert_eq!(class_devs_check(true, DIGCF_ALLCLASSES), Ok(()));
        assert_eq!(class_devs_check(false, 2), Ok(()));
        assert_eq!(enum_devinfo_error(false, Some(28), false), 6);
        assert_eq!(enum_devinfo_error(true, None, false), 87);
        assert_eq!(enum_devinfo_error(true, Some(0), false), 0x6f8);
        assert_eq!(enum_devinfo_error(true, Some(32), false), 0x6f8);
        assert_eq!(enum_devinfo_error(true, Some(28), false), 259);
        assert_eq!(enum_devinfo_error(true, Some(32), true), 259);
    }

    proptest! {
        /// the checks go handle, pointer, size, index: an earlier failure hides the later ones
        #[test]
        fn devinfo_check_order(set_ok: bool, data in proptest::option::of(0u32..64), x64: bool) {
            let e = enum_devinfo_error(set_ok, data, x64);
            if !set_ok { prop_assert_eq!(e, 6); }
            else if data.is_none() { prop_assert_eq!(e, 87); }
            else if data != Some(if x64 { 32 } else { 28 }) { prop_assert_eq!(e, 0x6f8); }
            else { prop_assert_eq!(e, 259); }
        }
    }

    #[test]
    fn windows_answers() {
        // Wine's cfgmgr32 tests (test_CM_Get_Device_Interface_List(_Size), test_devnode)
        assert_eq!(list_size(false, true, Instance::None, 0).0, CR_INVALID_POINTER);
        assert_eq!(list_size(true, false, Instance::None, 0).0, CR_FAILURE);
        assert_eq!(list_size(true, true, Instance::None, 0), (CR_SUCCESS, Some(1)));
        assert_eq!(list_size(true, true, Instance::None, 1), (CR_SUCCESS, Some(1)));
        assert_eq!(list_size(true, true, classify(Some(&w("INVALID"))), 1), (CR_INVALID_DEVNODE, Some(0)));
        assert_eq!(list_size(true, true, classify(Some(&w("\\\\?\\"))), 1).0, CR_INVALID_DEVNODE);
        assert_eq!(list_size(true, true, classify(Some(&w(""))), 1), (CR_SUCCESS, Some(1)));
        assert_eq!(list(true, false, 0, Instance::None, 0).0, CR_INVALID_POINTER);
        assert_eq!(list(false, true, 1, Instance::None, 0).0, CR_FAILURE);
        assert_eq!(list(true, true, 0, Instance::None, 0).0, CR_BUFFER_SMALL);
        assert_eq!(list(true, true, 1, Instance::None, 0), (CR_SUCCESS, true));
        assert_eq!(list(true, true, 1, classify(Some(&w("INVALID"))), 1).0, CR_INVALID_DEVNODE);
        assert_eq!(locate(classify(None)), (CR_SUCCESS, ROOT));
        assert_eq!(locate(classify(Some(&w("")))), (CR_SUCCESS, ROOT));
        assert_eq!(locate(classify(Some(&w("HTREE\\ROOT\\0")))), (CR_SUCCESS, ROOT));
        assert_eq!(locate(classify(Some(&w("htree\\root\\0")))), (CR_SUCCESS, ROOT));
        assert_eq!(locate(classify(Some(&w("INVALID")))), (CR_INVALID_DEVICE_ID, 0));
        assert_eq!(locate(classify(Some(&w("WINETEST\\WINETEST\\0123456")))), (CR_NO_SUCH_DEVNODE, 0));
        assert_eq!(cr_to_win32(CR_NO_SUCH_DEVICE_INTERFACE, 99), 1168);
        assert_eq!(cr_to_win32(CR_BUFFER_SMALL, 99), 122);
        assert_eq!(cr_to_win32(0x7777, 99), 99);
        // user32: Wine's RegisterDeviceNotificationW checks
        assert_eq!(devnotify_check(0, true, Some(DBT_DEVTYP_DEVICEINTERFACE)), Ok(()));
        assert_eq!(devnotify_check(4, true, Some(DBT_DEVTYP_DEVICEINTERFACE)), Ok(()));
        assert_eq!(devnotify_check(0, false, None), Err(87));
        assert_eq!(devnotify_check(1, false, None), Ok(()));
        assert_eq!(devnotify_check(2, true, None), Err(87));
        assert_eq!(devnotify_check(0, true, Some(3)), Err(13));
        // {4d1e55b2-f16f-11cf-88cb-001111000030} as the GUID struct's bytes
        let g = &GUID_DEVINTERFACE_HID;
        assert_eq!(u32::from_le_bytes(g[0..4].try_into().unwrap()), 0x4d1e_55b2);
        assert_eq!((u16::from_le_bytes([g[4], g[5]]), u16::from_le_bytes([g[6], g[7]])), (0xf16f, 0x11cf));
        assert_eq!(g[8..], [0x88, 0xcb, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30]);
    }

    proptest! {
        // flags: only CM_GET_DEVICE_INTERFACE_LIST_ALL_DEVICES (bit 0) is taken; any other bit
        // is CR_INVALID_FLAG once the pointers are good, and nothing is written then
        #[test]
        fn flags_and_pointer_order(len in any::<bool>(), class in any::<bool>(), buf in any::<bool>(), n in 0u32..8, flags in any::<u32>()) {
            let (cr, out) = list_size(len, class, Instance::None, flags);
            let want = if !class { CR_FAILURE } else if !len { CR_INVALID_POINTER } else if flags > 1 { CR_INVALID_FLAG } else { CR_SUCCESS };
            prop_assert_eq!(cr, want);
            prop_assert_eq!(out.is_some(), cr == CR_SUCCESS);
            let (cr, clear) = list(class, buf, n, Instance::None, flags);
            let want = if !class { CR_FAILURE } else if !buf { CR_INVALID_POINTER } else if n == 0 { CR_BUFFER_SMALL } else if flags > 1 { CR_INVALID_FLAG } else { CR_SUCCESS };
            prop_assert_eq!(cr, want);
            prop_assert_eq!(clear, cr == CR_SUCCESS);
        }

        // an id is the root (any case), well-formed but unknown (has a backslash), or malformed;
        // the size query and the list agree on every id, and an id that locates no node is never
        // listed as present
        #[test]
        fn ids_agree(id in "[A-Za-z0-9\\\\#?&_]{0,24}") {
            let v = w(&id);
            let inst = classify(Some(&v));
            let (lcr, _) = list_size(true, true, inst, 0);
            let (cr, _) = list(true, true, 4, inst, 0);
            prop_assert_eq!(lcr, cr);
            let (loc, node) = locate(inst);
            prop_assert_eq!(loc == CR_SUCCESS, cr == CR_SUCCESS);
            prop_assert_eq!(node != 0, loc == CR_SUCCESS);
            if !id.is_empty() && !id.contains('\\') { prop_assert_eq!(loc, CR_INVALID_DEVICE_ID); }
        }

        // the table maps every CONFIGRET it knows away from the default, and nothing else
        #[test]
        fn map_default(cr in 0u32..0x40, d in any::<u32>()) {
            let known = [0u32, 2, 3, 4, 5, 0x0d, 0x10, 0x1a, 0x1c, 0x1d, 0x1e, 0x25, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x37, 0x38];
            let got = cr_to_win32(cr, d);
            if known.contains(&cr) { prop_assert_eq!(got, cr_to_win32(cr, !d)); } else { prop_assert_eq!(got, d); }
        }
    }
}
