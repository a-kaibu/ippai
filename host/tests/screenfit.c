/* What the page shows of a game's window, and where its clicks go (tests/screen-browser.mjs).
 * argv[1]:
 *   gdi     a 640x360 popup (client = window) at (1000, 200) of sake's 1366x768 screen: 366 of
 *           its 640 columns off the screen, as AMBITIOUS MISSION after episode 1's window at
 *           x 850, where a bigger screen had it. Left half red, right half blue, its last 8
 *           columns green (all off the screen).
 *   d3d9    a captioned window with a 640x360 client at (100, 100) and a Direct3D 9 device of
 *           1024x768 (bigger than the client: 俺の姿が､透明に! makes it at the desktop's
 *           size): the back buffer blue, its top left 640x360 red, presented with that as the
 *           source rectangle.
 *   d3d9off the same device in the gdi window's place (partly off the screen).
 *   max     the gdi popup maximized by a posted WM_SYSCOMMAND SC_MAXIMIZE, as KiriKiri's
 *           windowEx plugin restores AMBITIOUS MISSION after episode 1's saved maximized
 *           window: its client the whole screen, left half red, right half blue.
 * Prints "ready" once drawn, then "click x,y" (client coordinates) for each left click, until
 * 120 s have passed. libs: -lgdi32 -ld3d9 */
#include <windows.h>
#include <d3d9.h>
#include <stdio.h>
#include <string.h>

static int mode; /* 0 gdi, 1 d3d9, 2 d3d9off, 3 max */
static IDirect3DDevice9 *dev;

static void paint(HWND h) {
    PAINTSTRUCT ps;
    HDC dc = BeginPaint(h, &ps);
    RECT l = { 0, 0, 320, 360 }, r = { 320, 0, 640, 360 }, g = { 632, 0, 640, 360 };
    if (mode == 3) {
        RECT c;
        GetClientRect(h, &c);
        l.right = r.left = c.right / 2;
        l.bottom = r.bottom = c.bottom;
        r.right = c.right;
        g = r;
        g.left = g.right;
    }
    HBRUSH red = CreateSolidBrush(RGB(255, 0, 0)), blue = CreateSolidBrush(RGB(0, 0, 255)), green = CreateSolidBrush(RGB(0, 255, 0));
    FillRect(dc, &l, red);
    FillRect(dc, &r, blue);
    FillRect(dc, &g, green);
    DeleteObject(red);
    DeleteObject(blue);
    DeleteObject(green);
    EndPaint(h, &ps);
}

static void frame(void) {
    D3DRECT part = { 0, 0, 640, 360 };
    RECT src = { 0, 0, 640, 360 };
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(0, 0, 255), 1.0f, 0);
    IDirect3DDevice9_Clear(dev, 1, &part, D3DCLEAR_TARGET, D3DCOLOR_XRGB(255, 0, 0), 1.0f, 0);
    IDirect3DDevice9_Present(dev, &src, NULL, NULL, NULL);
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    switch (m) {
    case WM_PAINT:
        if (mode == 0 || mode == 3) {
            paint(h);
            return 0;
        }
        break;
    case WM_LBUTTONDOWN:
        printf("click %d,%d\n", (short)LOWORD(l), (short)HIWORD(l));
        fflush(stdout);
        return 0;
    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

int main(int argc, char **argv) {
    const char *a = argc > 1 ? argv[1] : "";
    mode = !strcmp(a, "d3d9") ? 1 : !strcmp(a, "d3d9off") ? 2 : !strcmp(a, "max") ? 3 : 0;
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.lpszClassName = "screenfit";
    RegisterClassA(&wc);
    HWND h;
    if (mode == 1) {
        RECT r = { 0, 0, 640, 360 };
        AdjustWindowRect(&r, WS_OVERLAPPEDWINDOW, FALSE);
        h = CreateWindowA("screenfit", "screenfit", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 100 + r.left, 100 + r.top, r.right - r.left, r.bottom - r.top, NULL, NULL, wc.hInstance, NULL);
    } else {
        h = CreateWindowA("screenfit", "screenfit", WS_POPUP | WS_VISIBLE, 1000, 200, 640, 360, NULL, NULL, wc.hInstance, NULL);
    }
    if (mode == 3) {
        PostMessageA(h, WM_SYSCOMMAND, SC_MAXIMIZE, 0);
        MSG msg;
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&msg);
        printf("zoomed %d\n", IsZoomed(h) ? 1 : 0);
    }
    RECT c;
    GetClientRect(h, &c);
    POINT o = { 0, 0 };
    ClientToScreen(h, &o);
    printf("client %ldx%ld at %ld,%ld\n", c.right, c.bottom, o.x, o.y);
    if (mode == 1 || mode == 2) {
        IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
        D3DPRESENT_PARAMETERS pp = { 0 };
        pp.BackBufferWidth = 1024;
        pp.BackBufferHeight = 768;
        pp.BackBufferFormat = D3DFMT_X8R8G8B8;
        pp.BackBufferCount = 1;
        pp.SwapEffect = D3DSWAPEFFECT_COPY;
        pp.hDeviceWindow = h;
        pp.Windowed = TRUE;
        pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
        HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, h, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
        printf("CreateDevice %#lx\n", hr);
        if (FAILED(hr)) return 1;
        frame();
    } else {
        UpdateWindow(h);
    }
    printf("ready\n");
    fflush(stdout);
    DWORD end = GetTickCount() + 120000;
    while (GetTickCount() < end) {
        MSG msg;
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) return 0;
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
        if (mode == 1 || mode == 2) frame();
        Sleep(50);
    }
    return 0;
}
