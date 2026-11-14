/* DirectInput's system keyboard and mouse: GetDeviceInfo by the caller's dwSize (DirectX 3's
 * DIDEVICEINSTANCE_DX3A of 0x230 bytes and DirectX 5's 0x244, the wide ones on a W object, any
 * other size refused) and nothing written past it; device AddRef / Release counts; EnumDevices'
 * structure size for A and W callbacks.
 * libs: -ldinput8 -ldinput -ldxguid -luuid */
#define COBJMACROS
#define DIRECTINPUT_VERSION 0x0800
#include "../probe.h"
#include <dinput.h>

typedef HRESULT (WINAPI *Create)(HINSTANCE, DWORD, LPDIRECTINPUTA *, LPUNKNOWN);

static int past(const unsigned char *b, int from, int to) {
    for (int i = from; i < to; i++)
        if (b[i] != 0xaa) return i;
    return -1;
}

static void info(const char *name, IDirectInputDevice8A *d, int wide) {
    static const DWORD sizes[] = { 0x230, 0x244, 0x438, 0x44c, 0, 4, 0x200, 0x240, 0x248, 0x500 };
    for (int i = 0; i < sizeof sizes / sizeof *sizes; i++) {
        unsigned char b[0x600];
        memset(b, 0xaa, sizeof b);
        *(DWORD *)b = sizes[i];
        HRESULT hr = wide ? IDirectInputDevice8_GetDeviceInfo((IDirectInputDevice8W *)d, (DIDEVICEINSTANCEW *)b)
                          : IDirectInputDevice8_GetDeviceInfo(d, (DIDEVICEINSTANCEA *)b);
        int end = SUCCEEDED(hr) ? sizes[i] : 4;
        DIDEVICEINSTANCEA *a = (DIDEVICEINSTANCEA *)b;
        printf("%s GetDeviceInfo %#lx: %#lx dwSize %#lx type %#lx first byte past %d written past end %d\n", name, sizes[i], hr,
               *(DWORD *)b, SUCCEEDED(hr) ? a->dwDevType & 0xff : 0, end, past(b, end, sizeof b));
    }
}

static BOOL CALLBACK enum_a(LPCDIDEVICEINSTANCEA d, LPVOID ctx) {
    if ((d->dwDevType & 0xff) == DI8DEVTYPE_KEYBOARD || (d->dwDevType & 0xff) == DI8DEVTYPE_MOUSE)
        printf("%s enum: dwSize %#lx type %#lx\n", (const char *)ctx, d->dwSize, d->dwDevType & 0xff);
    return DIENUM_CONTINUE;
}

static void counts(const char *name, IDirectInputDevice8A *d) {
    ULONG a = IDirectInputDevice8_AddRef(d), b = IDirectInputDevice8_AddRef(d), c = IDirectInputDevice8_Release(d);
    ULONG e = IDirectInputDevice8_Release(d), f = IDirectInputDevice8_Release(d);
    printf("%s AddRef %lu %lu Release %lu %lu %lu\n", name, a, b, c, e, f);
}

int main(void) {
    HINSTANCE inst = GetModuleHandleA(NULL);
    IDirectInput8A *di8 = NULL;
    HRESULT hr = DirectInput8Create(inst, 0x0800, &IID_IDirectInput8A, (void **)&di8, NULL);
    printf("DirectInput8Create A %#lx\n", hr);
    if (FAILED(hr)) return 0;
    const GUID *g[2] = { &GUID_SysKeyboard, &GUID_SysMouse };
    const char *n[2] = { "keyboard8", "mouse8" };
    for (int k = 0; k < 2; k++) {
        IDirectInputDevice8A *d = NULL;
        hr = IDirectInput8_CreateDevice(di8, g[k], &d, NULL);
        printf("%s CreateDevice %#lx\n", n[k], hr);
        if (FAILED(hr)) continue;
        info(n[k], d, 0);
        counts(n[k], d);
    }
    IDirectInput8_EnumDevices(di8, DI8DEVCLASS_ALL, enum_a, "A8", DIEDFL_ATTACHEDONLY);
    IDirectInput8_Release(di8);

    IDirectInput8W *di8w = NULL;
    hr = DirectInput8Create(inst, 0x0800, &IID_IDirectInput8W, (void **)&di8w, NULL);
    printf("DirectInput8Create W %#lx\n", hr);
    if (SUCCEEDED(hr)) {
        IDirectInputDevice8W *d = NULL;
        hr = IDirectInput8_CreateDevice(di8w, &GUID_SysKeyboard, &d, NULL);
        printf("keyboard8W CreateDevice %#lx\n", hr);
        if (SUCCEEDED(hr)) {
            info("keyboard8W", (IDirectInputDevice8A *)d, 1);
            IDirectInputDevice8_Release(d);
        }
        IDirectInput8_EnumDevices(di8w, DI8DEVCLASS_ALL, (LPDIENUMDEVICESCALLBACKW)enum_a, "W8", DIEDFL_ATTACHEDONLY);
        IDirectInput8_Release(di8w);
    }

    /* dinput.dll, DirectX 3: DirectInputCreateA 0x300 */
    HMODULE m = LoadLibraryA("dinput.dll");
    Create create = m ? (Create)GetProcAddress(m, "DirectInputCreateA") : NULL;
    printf("dinput.dll DirectInputCreateA %s\n", create ? "found" : "missing");
    if (!create) return 0;
    IDirectInputA *di = NULL;
    hr = create(inst, 0x0300, &di, NULL);
    printf("DirectInputCreateA 0x300 %#lx\n", hr);
    if (FAILED(hr)) return 0;
    IDirectInputDeviceA *d = NULL;
    hr = IDirectInput_CreateDevice(di, &GUID_SysKeyboard, &d, NULL);
    printf("keyboard3 CreateDevice %#lx\n", hr);
    if (SUCCEEDED(hr)) {
        static const DWORD sizes[] = { 0x230, 0x244, 0x100 };
        for (int i = 0; i < 3; i++) {
            unsigned char b[0x300];
            memset(b, 0xaa, sizeof b);
            *(DWORD *)b = sizes[i];
            hr = IDirectInputDevice_GetDeviceInfo(d, (DIDEVICEINSTANCEA *)b);
            int end = SUCCEEDED(hr) ? sizes[i] : 4;
            printf("keyboard3 GetDeviceInfo %#lx: %#lx written past end %d\n", sizes[i], hr, past(b, end, sizeof b));
        }
        printf("keyboard3 Release %lu\n", IDirectInputDevice_Release(d));
    }
    IDirectInput_Release(di);
    return 0;
}
