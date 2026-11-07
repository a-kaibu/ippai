/* Raw Input as Unity 2019+ uses it (it gives up on input, then crashes, when the registration
 * fails): the device list and info, registration, and the WM_INPUT events SendInput's mouse
 * buttons and keys make. The expected lines are what Windows gives (one mouse, one keyboard).
 * Built for x86 and x64. libs: -luser32
 * Build: i686-w64-mingw32-gcc -O2 rawinput.c -o rawinput.exe */
#include <stdio.h>
#include <windows.h>

static int inputs, legacy_keys, legacy_buttons;

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_INPUT)
    {
        UINT size = 0;
        GetRawInputData((HRAWINPUT)l, RID_INPUT, 0, &size, sizeof(RAWINPUTHEADER));
        RAWINPUT r;
        UINT got = GetRawInputData((HRAWINPUT)l, RID_INPUT, &r, &size, sizeof(RAWINPUTHEADER));
        inputs++;
        if (r.header.dwType == RIM_TYPEKEYBOARD)
            printf("WM_INPUT keyboard: size ok %d, vk %02x flags %x message %x\n", got == size && size == r.header.dwSize, r.data.keyboard.VKey,
                   r.data.keyboard.Flags, r.data.keyboard.Message);
        else if (r.header.dwType == RIM_TYPEMOUSE && r.data.mouse.usButtonFlags)
            printf("WM_INPUT mouse: size ok %d, buttons %x\n", got == size && size == r.header.dwSize, r.data.mouse.usButtonFlags);
    }
    if (m == WM_KEYDOWN || m == WM_KEYUP)
        legacy_keys++;
    if (m >= WM_LBUTTONDOWN && m <= WM_MBUTTONUP)
        legacy_buttons++;
    return DefWindowProcA(h, m, w, l);
}

static void pump(void)
{
    MSG m;
    for (int i = 0; i < 20; i++)
    {
        while (PeekMessageA(&m, 0, 0, 0, PM_REMOVE))
            DispatchMessageA(&m);
        Sleep(10);
    }
}

static void inject(void)
{
    INPUT in[6] = {0};
    in[0].type = INPUT_KEYBOARD, in[0].ki.wVk = 'A';
    in[1].type = INPUT_KEYBOARD, in[1].ki.wVk = 'A', in[1].ki.dwFlags = KEYEVENTF_KEYUP;
    in[2].type = INPUT_MOUSE, in[2].mi.dwFlags = MOUSEEVENTF_LEFTDOWN;
    in[3].type = INPUT_MOUSE, in[3].mi.dwFlags = MOUSEEVENTF_LEFTUP;
    in[4].type = INPUT_MOUSE, in[4].mi.dwFlags = MOUSEEVENTF_RIGHTDOWN;
    in[5].type = INPUT_MOUSE, in[5].mi.dwFlags = MOUSEEVENTF_RIGHTUP;
    printf("SendInput: %u\n", SendInput(6, in, sizeof(INPUT)));
    pump();
}

int main(void)
{
    UINT n = 0;
    int r = (int)GetRawInputDeviceList(0, &n, sizeof(RAWINPUTDEVICELIST));
    printf("device list, count: %d, %u devices\n", r, n);
    RAWINPUTDEVICELIST list[8];
    UINT small = 1;
    SetLastError(0);
    r = (int)GetRawInputDeviceList(list, &small, sizeof(list[0]));
    printf("device list, too small: %d, error %lu, %u\n", r, GetLastError(), small);
    n = 8;
    int got = (int)GetRawInputDeviceList(list, &n, sizeof(list[0]));
    int mice = 0, keyboards = 0;
    HANDLE mouse = 0;
    for (int i = 0; i < got; i++)
    {
        if (list[i].dwType == RIM_TYPEMOUSE)
            mice++, mouse = list[i].hDevice;
        if (list[i].dwType == RIM_TYPEKEYBOARD)
            keyboards++;
    }
    printf("device list: %d, mice %d keyboards %d\n", got, mice, keyboards);
    RID_DEVICE_INFO info = {sizeof(info)};
    UINT isz = sizeof(info);
    r = (int)GetRawInputDeviceInfoA(mouse, RIDI_DEVICEINFO, &info, &isz);
    printf("mouse info: %d, type %lu\n", r, info.dwType);
    UINT nsz = 0;
    GetRawInputDeviceInfoW(mouse, RIDI_DEVICENAME, 0, &nsz);
    WCHAR name[256];
    r = (int)GetRawInputDeviceInfoW(mouse, RIDI_DEVICENAME, name, &nsz);
    printf("mouse name: all %d chars, starts with \\\\?\\ %d\n", r == (int)nsz, name[0] == '\\' && name[1] == '\\' && name[2] == '?' && name[3] == '\\');

    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.lpszClassName = "raw";
    RegisterClassA(&wc);
    HWND w = CreateWindowA("raw", "raw", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 320, 240, 0, 0, 0, 0);
    SetCursorPos(100, 120); /* in the client area */
    SetForegroundWindow(w);
    SetFocus(w);
    pump();

    RAWINPUTDEVICE d[2] = {{1, 2, 0, w}, {1, 6, 0, w}};
    SetLastError(0);
    r = (int)RegisterRawInputDevices(d, 2, 4);
    printf("register, bad size: %d, error %lu\n", r, GetLastError());
    printf("register: %d\n", (int)RegisterRawInputDevices(d, 2, sizeof(d[0])));
    RAWINPUTDEVICE back[4];
    UINT nb = 4;
    printf("registered: %d\n", (int)GetRegisteredRawInputDevices(back, &nb, sizeof(back[0])));
    inject();
    printf("WM_INPUT %d, legacy keys %d, legacy buttons %d\n", inputs, legacy_keys, legacy_buttons);

    /* no legacy keyboard messages, then the keyboard removed */
    inputs = legacy_keys = legacy_buttons = 0;
    d[1].dwFlags = RIDEV_NOLEGACY;
    RegisterRawInputDevices(&d[1], 1, sizeof(d[0]));
    inject();
    printf("keyboard no legacy: WM_INPUT %d, legacy keys %d, legacy buttons %d\n", inputs, legacy_keys, legacy_buttons);
    inputs = legacy_keys = legacy_buttons = 0;
    d[1].dwFlags = RIDEV_REMOVE;
    d[1].hwndTarget = 0;
    printf("remove: %d\n", (int)RegisterRawInputDevices(&d[1], 1, sizeof(d[0])));
    nb = 4;
    printf("registered: %d\n", (int)GetRegisteredRawInputDevices(back, &nb, sizeof(back[0])));
    inject();
    printf("keyboard removed: WM_INPUT %d, legacy keys %d, legacy buttons %d\n", inputs, legacy_keys, legacy_buttons);
    return 0;
}
