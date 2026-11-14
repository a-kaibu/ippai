/* The mouse as a pointer (EnableMouseInPointer), as Unity 6's Input System takes the mouse:
 * after the call, each button and wheel event comes as a WM_POINTER* message (the mouse's
 * pointer, PT_MOUSE) before the mouse message DefWindowProc makes from it, and GetPointerType /
 * GetPointerInfo describe it. Updates are left out (Windows may send more of them).
 * Built for x86 and x64. libs: -luser32
 * Build: i686-w64-mingw32-gcc -O2 mousepointer.c -o mousepointer.exe */
#define _WIN32_WINNT 0x0602
#include <stdio.h>
#include <windows.h>

static HWND win;

static const char *name(UINT m)
{
    switch (m)
    {
    case WM_POINTERDOWN: return "WM_POINTERDOWN";
    case WM_POINTERUP: return "WM_POINTERUP";
    case WM_POINTERWHEEL: return "WM_POINTERWHEEL";
    case WM_LBUTTONDOWN: return "WM_LBUTTONDOWN";
    case WM_LBUTTONUP: return "WM_LBUTTONUP";
    case WM_RBUTTONDOWN: return "WM_RBUTTONDOWN";
    case WM_RBUTTONUP: return "WM_RBUTTONUP";
    case WM_MOUSEWHEEL: return "WM_MOUSEWHEEL";
    }
    return 0;
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    const char *n = name(m);
    if (n && m >= WM_POINTERUPDATE && m <= WM_POINTERHWHEEL)
    {
        UINT32 id = GET_POINTERID_WPARAM(w);
        POINTER_INPUT_TYPE type = 0;
        BOOL t = GetPointerType(id, &type);
        POINTER_INFO pi;
        memset(&pi, 0xcc, sizeof(pi));
        BOOL i = GetPointerInfo(id, &pi);
        POINT c;
        GetCursorPos(&c);
        if (m == WM_POINTERWHEEL)
            printf("%s: delta %d", n, GET_WHEEL_DELTA_WPARAM(w));
        else
            printf("%s: flags %04x", n, (unsigned)(HIWORD(w) & (POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON |
                                                                  POINTER_FLAG_SECONDBUTTON | POINTER_FLAG_THIRDBUTTON | POINTER_FLAG_PRIMARY)));
        printf(", at the cursor %d, type %d %lu, info %d: type %lu, same id %d, down/up/wheel %lx, change %d, target %d, at the cursor %d, wheel %d\n",
               (short)LOWORD(l) == c.x && (short)HIWORD(l) == c.y, t, (unsigned long)type, i, (unsigned long)pi.pointerType, pi.pointerId == id,
               (unsigned long)(pi.pointerFlags & (POINTER_FLAG_DOWN | POINTER_FLAG_UP | POINTER_FLAG_WHEEL)), (int)pi.ButtonChangeType,
               pi.hwndTarget == h, pi.ptPixelLocation.x == c.x && pi.ptPixelLocation.y == c.y, (int)pi.InputData);
    }
    else if (n)
        printf("%s\n", n);
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

static void inject(DWORD flags, DWORD data)
{
    INPUT in = {0};
    in.type = INPUT_MOUSE;
    in.mi.dwFlags = flags;
    in.mi.mouseData = data;
    SendInput(1, &in, sizeof(in));
    pump();
}

int main(void)
{
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.lpszClassName = "ptr";
    RegisterClassA(&wc);
    win = CreateWindowA("ptr", "ptr", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 320, 240, 0, 0, 0, 0);
    SetCursorPos(100, 120); /* in the client area */
    SetForegroundWindow(win);
    SetFocus(win);
    pump();

    printf("in pointer before: %d\n", IsMouseInPointerEnabled());
    printf("a mouse click before:\n");
    inject(MOUSEEVENTF_LEFTDOWN, 0);
    inject(MOUSEEVENTF_LEFTUP, 0);

    printf("enable: %d\n", EnableMouseInPointer(TRUE));
    printf("in pointer: %d\n", IsMouseInPointerEnabled());
    printf("left click:\n");
    inject(MOUSEEVENTF_LEFTDOWN, 0);
    inject(MOUSEEVENTF_LEFTUP, 0);
    printf("right click:\n");
    inject(MOUSEEVENTF_RIGHTDOWN, 0);
    inject(MOUSEEVENTF_RIGHTUP, 0);
    printf("wheel:\n");
    inject(MOUSEEVENTF_WHEEL, (DWORD)-120);
    DestroyWindow(win);
    return 0;
}
