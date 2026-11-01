/* DirectDraw check for vnvm: draws moving color bars into an offscreen surface (Lock), writes text
 * on it (GetDC) and blits it to the window through a clipper; with "full" as the argument it
 * switches to exclusive 640x480x32 with a flip chain instead (how old VN engines draw). Prints
 * each step and quits after 60 s.
 * Build: i686-w64-mingw32-gcc -O2 ddrawtest.c -o ddrawtest.exe -mwindows -lddraw -ldxguid -lgdi32 */
#include <windows.h>
#include <ddraw.h>
#include <stdio.h>
#include <string.h>

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

#define CHECK(x) do { HRESULT hr_ = (x); printf("%s -> %#lx\n", #x, (unsigned long)hr_); fflush(stdout); if (FAILED(hr_)) return 1; } while (0)

static void paint(IDirectDrawSurface7 *s, int w, int h, int t)
{
    DDSURFACEDESC2 sd = { sizeof(sd) };
    HDC dc;
    char text[64];
    if (SUCCEEDED(IDirectDrawSurface7_Lock(s, NULL, &sd, DDLOCK_WAIT | DDLOCK_WRITEONLY, 0)))
    {
        for (int y = 0; y < h; y++)
        {
            DWORD *row = (DWORD *)((BYTE *)sd.lpSurface + y * sd.lPitch);
            for (int x = 0; x < w; x++)
            {
                int band = ((x + t * 4) / 80) % 6;
                static const DWORD colors[6] = { 0xff0000, 0x00ff00, 0x0000ff, 0xffff00, 0x00ffff, 0xff00ff };
                row[x] = y < h / 2 ? colors[band] : (DWORD)((x * 255 / w) << 8 | (y * 255 / h));
            }
        }
        IDirectDrawSurface7_Unlock(s, NULL);
    }
    if (SUCCEEDED(IDirectDrawSurface7_GetDC(s, &dc)))
    {
        sprintf(text, "DirectDraw frame %d", t);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, RGB(255, 255, 255));
        TextOutA(dc, 20, 20, text, strlen(text));
        IDirectDrawSurface7_ReleaseDC(s, dc);
    }
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    BOOL full = strstr(cmd, "full") != NULL;
    WNDCLASSA wc = { 0, wndproc, 0, 0, inst, 0, LoadCursor(0, IDC_ARROW), 0, 0, "ddrawtest" };
    IDirectDraw7 *dd;
    IDirectDrawSurface7 *primary, *back = NULL, *off;
    IDirectDrawClipper *clip;
    DDSURFACEDESC2 sd = { sizeof(sd) };
    HWND hwnd;
    DWORD start = GetTickCount();
    int w = 640, h = 480;
    MSG msg;

    RegisterClassA(&wc);
    hwnd = CreateWindowA("ddrawtest", "ddrawtest", full ? WS_POPUP | WS_VISIBLE : WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                         0, 0, full ? w : w + 16, full ? h : h + 39, 0, 0, inst, 0);
    CHECK(DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL));
    if (full)
    {
        CHECK(IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_EXCLUSIVE | DDSCL_FULLSCREEN));
        CHECK(IDirectDraw7_SetDisplayMode(dd, w, h, 32, 0, 0));
        sd.dwFlags = DDSD_CAPS | DDSD_BACKBUFFERCOUNT;
        sd.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE | DDSCAPS_FLIP | DDSCAPS_COMPLEX;
        sd.dwBackBufferCount = 1;
        CHECK(IDirectDraw7_CreateSurface(dd, &sd, &primary, NULL));
        DDSCAPS2 caps = { DDSCAPS_BACKBUFFER };
        CHECK(IDirectDrawSurface7_GetAttachedSurface(primary, &caps, &back));
    }
    else
    {
        CHECK(IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL));
        sd.dwFlags = DDSD_CAPS;
        sd.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
        CHECK(IDirectDraw7_CreateSurface(dd, &sd, &primary, NULL));
        CHECK(IDirectDraw7_CreateClipper(dd, 0, &clip, NULL));
        CHECK(IDirectDrawClipper_SetHWnd(clip, 0, hwnd));
        CHECK(IDirectDrawSurface7_SetClipper(primary, clip));
    }
    memset(&sd, 0, sizeof(sd));
    sd.dwSize = sizeof(sd);
    sd.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    sd.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    sd.dwWidth = w;
    sd.dwHeight = h;
    sd.ddpfPixelFormat.dwSize = sizeof(sd.ddpfPixelFormat);
    sd.ddpfPixelFormat.dwFlags = DDPF_RGB;
    sd.ddpfPixelFormat.dwRGBBitCount = 32;
    sd.ddpfPixelFormat.dwRBitMask = 0xff0000;
    sd.ddpfPixelFormat.dwGBitMask = 0x00ff00;
    sd.ddpfPixelFormat.dwBBitMask = 0x0000ff;
    CHECK(IDirectDraw7_CreateSurface(dd, &sd, &off, NULL));

    for (int t = 0; GetTickCount() - start < 60000; t++)
    {
        while (PeekMessageA(&msg, 0, 0, 0, PM_REMOVE))
        {
            if (msg.message == WM_QUIT) return 0;
            DispatchMessageA(&msg);
        }
        paint(off, w, h, t);
        if (full)
        {
            IDirectDrawSurface7_BltFast(back, 0, 0, off, NULL, DDBLTFAST_WAIT);
            IDirectDrawSurface7_Flip(primary, NULL, DDFLIP_WAIT);
        }
        else
        {
            RECT r;
            POINT p = { 0, 0 };
            GetClientRect(hwnd, &r);
            ClientToScreen(hwnd, &p);
            OffsetRect(&r, p.x, p.y);
            IDirectDrawSurface7_Blt(primary, &r, off, NULL, DDBLT_WAIT, NULL);
        }
        if (t % 60 == 0)
        {
            printf("frame %d at %lu ms\n", t, GetTickCount() - start);
            fflush(stdout);
        }
        Sleep(16);
    }
    return 0;
}
