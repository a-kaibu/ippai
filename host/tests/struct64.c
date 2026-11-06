/* Structures whose layout differs on x64 (pointer-sized fields): SYSTEM_INFO,
 * MEMORY_BASIC_INFORMATION, BITMAP / DIBSECTION, PAINTSTRUCT. Prints what a program reads
 * from them; built for both (the same lines are expected from each). No C runtime (x64 has
 * no msvcrt of sake's own yet): wsprintf and WriteFile.
 * Build: x86_64-w64-mingw32-gcc -O2 -nostdlib struct64.c -o struct64.exe -lgdi32 -luser32 -lkernel32
 *        (i686-w64-mingw32-gcc the same for 32-bit; tests/host-tests.sh runs both)
 * libs: -nostdlib -lgdi32 -luser32 -lkernel32 */
#include <windows.h>

void *memset(void *d, int c, size_t n)
{
    char *p = d;
    while (n--) *p++ = (char)c;
    return d;
}

static void out(const char *f, ...)
{
    char b[512];
    DWORD n;
    va_list a;
    va_start(a, f);
    n = wvsprintfA(b, f, a);
    va_end(a);
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), b, n, &n, NULL);
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_PAINT)
    {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(h, &ps);
        out("paint dc %s rect %s\n", dc && ps.hdc == dc ? "ok" : "BAD",
            ps.rcPaint.left == 0 && ps.rcPaint.top == 0 && ps.rcPaint.right > 0 && ps.rcPaint.bottom > 0 ? "ok" : "BAD");
        EndPaint(h, &ps);
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

void mainCRTStartup(void)
{
    SYSTEM_INFO si;
    GetSystemInfo(&si);
    out("sysinfo page %lu gran %lu cpus %s min %s max %s\n", si.dwPageSize, si.dwAllocationGranularity,
           si.dwNumberOfProcessors >= 1 && si.dwActiveProcessorMask != 0 ? "ok" : "BAD",
           (size_t)si.lpMinimumApplicationAddress == 0x10000 ? "ok" : "BAD",
           (size_t)si.lpMaximumApplicationAddress > 0x10000000 ? "ok" : "BAD");

    char *p = VirtualAlloc(NULL, 3 * 4096, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    MEMORY_BASIC_INFORMATION mi;
    SIZE_T n = VirtualQuery(p + 4096, &mi, sizeof mi);
    out("query %s base %s alloc %s size %lu state %lx protect %lx type %lx\n", n == sizeof mi ? "ok" : "BAD",
           mi.BaseAddress == p + 4096 ? "ok" : "BAD", mi.AllocationBase == p ? "ok" : "BAD", (unsigned long)mi.RegionSize,
           mi.State, mi.Protect, mi.Type);

    BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), 64, -32, 1, 32, BI_RGB } };
    void *bits;
    HBITMAP hb = CreateDIBSection(NULL, &bi, DIB_RGB_COLORS, &bits, NULL, 0);
    BITMAP bm;
    DIBSECTION ds;
    int a = GetObjectA(hb, sizeof bm, &bm), b = GetObjectA(hb, sizeof ds, &ds);
    out("bitmap %s %ld x %ld bpp %d bits %s\n", a == sizeof bm ? "ok" : "BAD", bm.bmWidth, bm.bmHeight, bm.bmBitsPixel,
           bm.bmBits == bits ? "ok" : "BAD");
    out("dibsection %s %ld x %ld bits %s size %lu\n", b == sizeof ds ? "ok" : "BAD", ds.dsBmih.biWidth, ds.dsBmih.biHeight < 0 ? -ds.dsBmih.biHeight : ds.dsBmih.biHeight,
           ds.dsBm.bmBits == bits ? "ok" : "BAD", ds.dsBmih.biSizeImage);

    WNDCLASSA wc = { 0, proc, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "s64" };
    RegisterClassA(&wc);
    HWND w = CreateWindowA("s64", "s64", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 200, 100, 0, 0, wc.hInstance, 0);
    MSG msg;
    while (w && GetMessageA(&msg, 0, 0, 0) > 0) DispatchMessageA(&msg);
    ExitProcess(0);
}
