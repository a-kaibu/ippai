/* cfgmgr32.dll as a program finds it at run time (SDL3's HIDAPI, which Godot 4.6 uses for
 * joypads, #104): LoadLibrary succeeds, the device interface list of a class with no devices is
 * one NUL, and the argument checks come in Windows' order. Only answers that don't depend on the
 * machine's devices are printed (the HID class list's own contents vary: only that it ends in a
 * double NUL). RegisterDeviceNotification on a window gives a handle. */
#include "../probe.h"
#include <dbt.h>

typedef DWORD (WINAPI *size_w)(ULONG *, GUID *, WCHAR *, ULONG);
typedef DWORD (WINAPI *list_w)(GUID *, WCHAR *, WCHAR *, ULONG, ULONG);
typedef DWORD (WINAPI *size_a)(ULONG *, GUID *, char *, ULONG);
typedef DWORD (WINAPI *list_a)(GUID *, char *, char *, ULONG, ULONG);
typedef DWORD (WINAPI *locate_w)(DWORD *, WCHAR *, ULONG);
typedef DWORD (WINAPI *parent_f)(DWORD *, DWORD, ULONG);
typedef DWORD (WINAPI *map_f)(DWORD, DWORD);
typedef DWORD (WINAPI *iprop_f)(const WCHAR *, const void *, ULONG *, BYTE *, ULONG *, ULONG);
typedef DWORD (WINAPI *reg_f)(void *, void *, void *, HANDLE *);
typedef DWORD (WINAPI *unreg_f)(HANDLE);

static const GUID nothing = { 0x5ac8e0d3, 0x81b6, 0x4f4e, { 0x9e, 0x0c, 0x21, 0x7a, 0x41, 0x33, 0x0b, 0x6d } };
static const GUID hid = { 0x4d1e55b2, 0xf16f, 0x11cf, { 0x88, 0xcb, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30 } };

static DWORD CALLBACK cb(HANDLE n, void *ctx, DWORD action, void *data, DWORD size) { return 0; }

int main(void) {
    HMODULE m = LoadLibraryA("cfgmgr32.dll");
    printf("LoadLibrary %s\n", m ? "ok" : "failed");
    if (!m) return 1;
#define P(t, n) t n = (t)GetProcAddress(m, #n); printf("%-40s %s\n", #n, n ? "found" : "missing")
    P(size_w, CM_Get_Device_Interface_List_SizeW);
    P(list_w, CM_Get_Device_Interface_ListW);
    P(size_a, CM_Get_Device_Interface_List_SizeA);
    P(list_a, CM_Get_Device_Interface_ListA);
    P(locate_w, CM_Locate_DevNodeW);
    P(parent_f, CM_Get_Parent);
    P(map_f, CM_MapCrToWin32Err);
    P(iprop_f, CM_Get_Device_Interface_PropertyW);
    P(reg_f, CM_Register_Notification);
    P(unreg_f, CM_Unregister_Notification);
    GUID g = nothing, h = hid;
    ULONG len;
    WCHAR buf[8];
    char abuf[8];
    DWORD node, r;

    len = 77; printf("size(NULL len) = %#lx\n", CM_Get_Device_Interface_List_SizeW(NULL, &g, NULL, 0));
    len = 77; r = CM_Get_Device_Interface_List_SizeW(&len, NULL, NULL, 0); printf("size(NULL class) = %#lx len %lu\n", r, len);
    for (ULONG f = 1; f <= 8; f <<= 1) { len = 77; r = CM_Get_Device_Interface_List_SizeW(&len, &g, NULL, f); printf("size(flags %lu) = %#lx len %lu\n", f, r, len); }
    len = 77; r = CM_Get_Device_Interface_List_SizeW(&len, &g, L"", 0); printf("size(\"\") = %#lx len %lu\n", r, len);
    len = 77; r = CM_Get_Device_Interface_List_SizeW(&len, &g, L"INVALID", 1); printf("size(INVALID) = %#lx len %lu\n", r, len);
    len = 77; r = CM_Get_Device_Interface_List_SizeA(&len, &g, NULL, 0); printf("sizeA = %#lx len %lu\n", r, len);
    len = 0; r = CM_Get_Device_Interface_List_SizeW(&len, &h, NULL, 0);
    printf("size(HID) = %#lx, at least 1: %d\n", r, len >= 1);

    printf("list(NULL buffer) = %#lx\n", CM_Get_Device_Interface_ListW(&g, NULL, NULL, 0, 0));
    printf("list(NULL class) = %#lx\n", CM_Get_Device_Interface_ListW(NULL, NULL, buf, 8, 0));
    printf("list(len 0) = %#lx\n", CM_Get_Device_Interface_ListW(&g, NULL, buf, 0, 0));
    printf("list(flags 2) = %#lx\n", CM_Get_Device_Interface_ListW(&g, NULL, buf, 8, 2));
    for (int i = 0; i < 8; i++) buf[i] = 'x';
    r = CM_Get_Device_Interface_ListW(&g, NULL, buf, 8, 0);
    printf("list = %#lx:", r);
    for (int i = 0; i < 8; i++) printf(" %x", buf[i]);
    printf("\n");
    memset(abuf, 'x', 8);
    r = CM_Get_Device_Interface_ListA(&g, NULL, abuf, 8, 1);
    printf("listA = %#lx:", r);
    for (int i = 0; i < 8; i++) printf(" %x", (unsigned char)abuf[i]);
    printf("\n");
    printf("list(INVALID) = %#lx\n", CM_Get_Device_Interface_ListW(&g, L"INVALID", buf, 8, 1));
    {
        WCHAR *big; len = 0;
        CM_Get_Device_Interface_List_SizeW(&len, &h, NULL, 0);
        big = calloc(len + 1, sizeof(WCHAR));
        r = CM_Get_Device_Interface_ListW(&h, NULL, big, len, 0);
        printf("list(HID) = %#lx, ends in two NULs: %d\n", r, len == 1 ? big[0] == 0 : (big[len - 1] == 0 && big[len - 2] == 0));
        free(big);
    }

    printf("locate(NULL ptr) = %#lx\n", CM_Locate_DevNodeW(NULL, NULL, 0));
    node = 0xdead; r = CM_Locate_DevNodeW(&node, NULL, 0); printf("locate(NULL) = %#lx, node set: %d\n", r, node != 0 && node != 0xdead);
    DWORD root = node;
    node = 0xdead; r = CM_Locate_DevNodeW(&node, L"", 0); printf("locate(\"\") = %#lx, same: %d\n", r, node == root);
    node = 0xdead; r = CM_Locate_DevNodeW(&node, L"HTREE\\ROOT\\0", 0); printf("locate(root) = %#lx, same: %d\n", r, node == root);
    node = 0xdead; r = CM_Locate_DevNodeW(&node, L"INVALID", 0); printf("locate(INVALID) = %#lx node %#lx\n", r, node);
    node = 0xdead; r = CM_Locate_DevNodeW(&node, L"WINETEST\\WINETEST\\0123456", 0); printf("locate(unknown) = %#lx node %#lx\n", r, node);
    node = 0xdead; r = CM_Get_Parent(&node, root, 0); printf("parent(root) = %#lx node %#lx\n", r, node);
    printf("parent(NULL) = %#lx\n", CM_Get_Parent(NULL, root, 0));

    ULONG type, size = 0;
    BYTE pb[16];
    static const BYTE key[20] = { 0 };
    printf("iface property(NULL) = %#lx\n", CM_Get_Device_Interface_PropertyW(NULL, key, &type, pb, &size, 0));
    printf("iface property(none) = %#lx\n", CM_Get_Device_Interface_PropertyW(L"\\\\?\\WINETEST#NOTHING#1#{5ac8e0d3-81b6-4f4e-9e0c-217a41330b6d}", key, &type, pb, &size, 0));

    static const DWORD crs[] = { 0, 2, 3, 4, 5, 0xd, 0x10, 0x13, 0x1a, 0x1c, 0x1d, 0x1e, 0x1f, 0x25, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x37, 0x38, 0x3b };
    for (int i = 0; i < sizeof(crs) / sizeof(crs[0]); i++) printf("map %#lx = %lu\n", crs[i], CM_MapCrToWin32Err(crs[i], 4242));

    struct { DWORD cb, flags, type, reserved; WCHAR u[200]; } filter;
    memset(&filter, 0, sizeof(filter));
    filter.cb = sizeof(filter);
    filter.flags = 1; /* CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES */
    HANDLE n = NULL;
    printf("filter size %u\n", (unsigned)sizeof(filter));
    printf("register(NULL out) = %#lx\n", CM_Register_Notification(&filter, NULL, cb, NULL));
    printf("register(NULL cb) = %#lx\n", CM_Register_Notification(&filter, NULL, NULL, &n));
    filter.cb = 8; printf("register(bad size) = %#lx\n", CM_Register_Notification(&filter, NULL, cb, &n)); filter.cb = sizeof(filter);
    r = CM_Register_Notification(&filter, NULL, cb, &n);
    printf("register = %#lx handle %d\n", r, n != NULL);
    printf("unregister = %#lx\n", CM_Unregister_Notification(n));
    printf("unregister(NULL) = %#lx\n", CM_Unregister_Notification(NULL));

    typedef void (WINAPI *hidguid_f)(GUID *);
    hidguid_f HidD_GetHidGuid = (hidguid_f)GetProcAddress(LoadLibraryA("hid.dll"), "HidD_GetHidGuid");
    GUID hg;
    memset(&hg, 0, sizeof(hg));
    if (HidD_GetHidGuid) HidD_GetHidGuid(&hg);
    printf("HidD_GetHidGuid: %d is GUID_DEVINTERFACE_HID: %d\n", HidD_GetHidGuid != NULL, !memcmp(&hg, &hid, sizeof(hg)));

    HWND w = CreateWindowA("STATIC", "cfgmgr", 0, 0, 0, 10, 10, NULL, NULL, NULL, NULL);
    DEV_BROADCAST_DEVICEINTERFACE_W di;
    memset(&di, 0, sizeof(di));
    di.dbcc_size = sizeof(di);
    di.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
    di.dbcc_classguid = hid;
    SetLastError(0xdead);
    HDEVNOTIFY dn = RegisterDeviceNotificationW(w, &di, DEVICE_NOTIFY_WINDOW_HANDLE);
    printf("RegisterDeviceNotificationW = %d err %lu\n", dn != NULL, dn ? 0 : GetLastError());
    SetLastError(0xdead);
    printf("UnregisterDeviceNotification = %d\n", UnregisterDeviceNotification(dn));
    SetLastError(0xdead);
    HDEVNOTIFY d2 = RegisterDeviceNotificationW(w, &di, DEVICE_NOTIFY_WINDOW_HANDLE | DEVICE_NOTIFY_ALL_INTERFACE_CLASSES);
    printf("RegisterDeviceNotificationW(all) = %d err %lu\n", d2 != NULL, d2 ? 0 : GetLastError());
    UnregisterDeviceNotification(d2);
    SetLastError(0xdead);
    dn = RegisterDeviceNotificationW(w, &di, 0x10);
    printf("RegisterDeviceNotificationW(flags 0x10) = %d err %lu\n", dn != NULL, GetLastError());
    SetLastError(0xdead);
    dn = RegisterDeviceNotificationW((HWND)0x1234, &di, DEVICE_NOTIFY_WINDOW_HANDLE);
    printf("RegisterDeviceNotificationW(no window) = %d err %lu\n", dn != NULL, GetLastError());
    DestroyWindow(w);
    return 0;
}
