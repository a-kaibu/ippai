/* More out-parameters that are pointer-sized (8 bytes on x64), found by sake's
 * outptr_audit test from Microsoft's Win32 metadata (tools/win32md): sake wrote their low half
 * only. Same protocol as outptr.c: each variable starts as 0xcd bytes; "half" says that its upper
 * half (x64) or the whole of it (x86) was left as it was.
 * libs: -lole32 -loleaut32 -lddraw -ldsound -lwinmm -lmsacm32 -lgdiplus -lbcrypt -ldxguid -luuid */
#include "../probe.h"
#include <objbase.h>
#include <oleauto.h>
#include <ddraw.h>
#include <dsound.h>
#include <mmreg.h>
#include <msacm.h>
#include <bcrypt.h>

#define CANARY ((ULONG_PTR)0xcdcdcdcdcdcdcdcdull)
static int half(ULONG_PTR v) {
#ifdef _WIN64
    return (v >> 32) == 0xcdcdcdcdu;
#else
    return v == CANARY;
#endif
}
#define CHECK(name, call, var)                                                                                                     \
    do {                                                                                                                         \
        memset(&(var), 0xcd, sizeof(var));                                                                                       \
        LONG_PTR r = (LONG_PTR)(call);                                                                                           \
        ULONG_PTR v_;                                                                                                            \
        memcpy(&v_, &(var), sizeof(v_));                                                                                         \
        printf("%s: ret %s half %d null %d\n", name, r == 0 ? "0" : r == 1 ? "1" : "other", half(v_), v_ == 0);                 \
    } while (0)

/* GDI+'s flat API (mingw's gdiplus.h is C++) */
typedef struct {
    UINT32 GdiplusVersion;
    void *DebugEventCallback;
    BOOL SuppressBackgroundThread, SuppressExternalCodecs;
} GSI;
int WINAPI GdiplusStartup(ULONG_PTR *, const GSI *, void *);
void WINAPI GdiplusShutdown(ULONG_PTR);
int WINAPI GdipCreateBitmapFromScan0(INT, INT, INT, INT, BYTE *, void **);
int WINAPI GdipCreateHBITMAPFromBitmap(void *, HBITMAP *, DWORD);
int WINAPI GdipDisposeImage(void *);

static HACMDRIVERID first_driver;
static BOOL CALLBACK driver_cb(HACMDRIVERID id, DWORD_PTR inst, DWORD support) {
    if (!first_driver) first_driver = id;
    return TRUE;
}

int main(void) {
    /* kernel32 */
    DWORD_PTR pm, sm;
    memset(&sm, 0xcd, sizeof sm);
    CHECK("GetProcessAffinityMask process", GetProcessAffinityMask(GetCurrentProcess(), &pm, &sm), pm);
    printf("GetProcessAffinityMask system: half %d\n", half(sm));
    SRWLOCK srw;
    CHECK("InitializeSRWLock", (InitializeSRWLock(&srw), 0), srw);
    CONDITION_VARIABLE cv;
    CHECK("InitializeConditionVariable", (InitializeConditionVariable(&cv), 0), cv);
    INIT_ONCE once;
    CHECK("InitOnceInitialize", (InitOnceInitialize(&once), 0), once);

    /* user32 */
    HKL kl[2];
    CHECK("GetKeyboardLayoutList", GetKeyboardLayoutList(1, kl) == 1, kl[0]);

    /* bcrypt */
    BCRYPT_ALG_HANDLE alg;
    CHECK("BCryptOpenAlgorithmProvider", BCryptOpenAlgorithmProvider(&alg, L"SHA256", NULL, 0), alg);

    /* ole */
    IErrorInfo *ei;
    CHECK("GetErrorInfo", GetErrorInfo(0, &ei), ei);
    static const GUID nolib = { 0x5a1e0001, 0x1234, 0x5678, { 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78 } };
    ITypeLib *tl;
    CHECK("LoadRegTypeLib unregistered", LoadRegTypeLib(&nolib, 1, 0, 0, &tl) == 0, tl);

    /* GDI+ */
    ULONG_PTR token;
    GSI si = { 1, NULL, FALSE, FALSE };
    CHECK("GdiplusStartup", GdiplusStartup(&token, &si, NULL), token);
    void *bmp = NULL;
    GdipCreateBitmapFromScan0(4, 4, 0, 0x26200a /* PixelFormat32bppARGB */, NULL, &bmp);
    HBITMAP hb;
    CHECK("GdipCreateHBITMAPFromBitmap", GdipCreateHBITMAPFromBitmap(bmp, &hb, 0), hb);

    /* DirectDraw */
    IDirectDraw *dd;
    CHECK("DirectDrawCreate", DirectDrawCreate(NULL, &dd, NULL), dd);
    void *dd7;
    CHECK("DirectDrawCreateEx", DirectDrawCreateEx(NULL, &dd7, &IID_IDirectDraw7, NULL), dd7);
    IDirectDrawClipper *clip;
    CHECK("DirectDrawCreateClipper", DirectDrawCreateClipper(0, &clip, NULL), clip);

    /* winmm, msacm32, dsound: the result depends on the machine's audio; whether the
     * variable is written whole does not */
    HMIXER mx;
    memset(&mx, 0xcd, sizeof mx);
    mixerOpen(&mx, 0, 0, 0, MIXER_OBJECTF_MIXER);
    printf("mixerOpen: half %d\n", half((ULONG_PTR)mx));
    acmDriverEnum(driver_cb, 0, 0);
    if (first_driver) {
        HACMDRIVER had;
        memset(&had, 0xcd, sizeof had);
        MMRESULT r = acmDriverOpen(&had, first_driver, 0);
        printf("acmDriverOpen: ret %s half %d\n", r == 0 ? "0" : "other", half((ULONG_PTR)had));
        if (r == 0) {
            HACMDRIVERID id;
            memset(&id, 0xcd, sizeof id);
            r = acmDriverID((HACMOBJ)had, &id, 0);
            printf("acmDriverID: ret %s half %d same %d\n", r == 0 ? "0" : "other", half((ULONG_PTR)id), id == first_driver);
        }
    }
    IDirectSound *ds;
    memset(&ds, 0xcd, sizeof ds);
    DirectSoundCreate(NULL, &ds, NULL);
    printf("DirectSoundCreate: half %d\n", half((ULONG_PTR)ds));
    IDirectSound8 *ds8;
    memset(&ds8, 0xcd, sizeof ds8);
    DirectSoundCreate8(NULL, &ds8, NULL);
    printf("DirectSoundCreate8: half %d\n", half((ULONG_PTR)ds8));
    return 0;
}
