/* IDirectDraw::WaitForVerticalBlank as old games time it (MBTRIAL0930's mb_trial.exe takes the
 * performance counter around a wait with DDWAITVB_BLOCKBEGIN and divides by the difference):
 * each flag's result, and whether two waits in a row took time at all (the rate itself depends
 * on the runner's display, so only "some time passed" is printed).
 * libs: -lddraw -ldxguid
 * arch: 32 (DirectDraw games are 32-bit; sake has no x64 DirectDraw) */
#include "../probe.h"
#include <ddraw.h>

int main(void) {
    IDirectDraw *dd = NULL;
    HRESULT hr = DirectDrawCreate(NULL, &dd, NULL);
    if (FAILED(hr)) { printf("skip DirectDrawCreate %#lx\n", hr); return 0; }
    printf("SetCooperativeLevel %#lx\n", IDirectDraw_SetCooperativeLevel(dd, NULL, DDSCL_NORMAL));
    printf("BLOCKBEGIN %#lx\n", IDirectDraw_WaitForVerticalBlank(dd, DDWAITVB_BLOCKBEGIN, NULL));
    printf("BLOCKEND %#lx\n", IDirectDraw_WaitForVerticalBlank(dd, DDWAITVB_BLOCKEND, NULL));
    printf("BLOCKBEGINEVENT %#lx\n", IDirectDraw_WaitForVerticalBlank(dd, DDWAITVB_BLOCKBEGINEVENT, NULL));
    LARGE_INTEGER f, a, b;
    QueryPerformanceFrequency(&f);
    int positive = 1;
    for (int i = 0; i < 5; i++) {
        IDirectDraw_WaitForVerticalBlank(dd, DDWAITVB_BLOCKBEGIN, NULL);
        QueryPerformanceCounter(&a);
        IDirectDraw_WaitForVerticalBlank(dd, DDWAITVB_BLOCKBEGIN, NULL);
        QueryPerformanceCounter(&b);
        if (b.QuadPart - a.QuadPart <= 0) positive = 0;
    }
    printf("two waits take time %d\n", positive);
    BOOL vb = 7;
    printf("GetVerticalBlankStatus %#lx bool %d\n", IDirectDraw_GetVerticalBlankStatus(dd, &vb), vb == TRUE || vb == FALSE);
    IDirectDraw_Release(dd);
    return 0;
}
