/* libs: -luser32 -lcomctl32 */
/* The progress bar (msctls_progress32): its range, position and step messages and what each
 * returns (the old position, the old range as MAKELONG), positions outside the range clamped,
 * PBM_GETRANGE both ways, PBM_SETRANGE32 above 16 bits, the colours. The copy page of
 * せきさば！'s installer (Delphi's TProgressBar, #222) shows one; sake drew nothing and answered
 * every message 0. (sake: progress.rs) */
#include <stdio.h>
#include <windows.h>
#include <commctrl.h>

#define S(m, w, l) ((long)SendMessageA(p, m, (WPARAM)(w), (LPARAM)(l)))

int main(void)
{
    INITCOMMONCONTROLSEX icc = {sizeof icc, ICC_PROGRESS_CLASS};
    printf("InitCommonControlsEx %d\n", InitCommonControlsEx(&icc));
    HWND top = CreateWindowExA(0, "STATIC", "progress", WS_OVERLAPPEDWINDOW, 20, 20, 300, 120, NULL, NULL, NULL, NULL);
    HWND p = CreateWindowExA(0, PROGRESS_CLASSA, "", WS_CHILD | WS_VISIBLE, 10, 10, 200, 18, top, NULL, NULL, NULL);
    printf("created %d\n", p != NULL);
    { long a_ = S(PBM_GETPOS, 0, 0), b_ = S(PBM_GETRANGE, TRUE, 0), c_ = S(PBM_GETRANGE, FALSE, 0), d_ = S(PBM_GETSTEP, 0, 0); printf("default: pos %ld low %ld high %ld step %ld\n", a_, b_, c_, d_); }
    { long a_ = S(PBM_SETPOS, 30, 0); long b_ = S(PBM_GETPOS, 0, 0); printf("SETPOS 30: old %ld now %ld\n", a_, b_); }
    { long a_ = S(PBM_DELTAPOS, 15, 0); long b_ = S(PBM_GETPOS, 0, 0); printf("DELTAPOS 15: old %ld now %ld\n", a_, b_); }
    printf("SETSTEP 7: old %ld\n", S(PBM_SETSTEP, 7, 0));
    { long a_ = S(PBM_STEPIT, 0, 0); long b_ = S(PBM_GETPOS, 0, 0); printf("STEPIT: old %ld now %ld\n", a_, b_); }
    { long a_ = S(PBM_SETPOS, 500, 0); long b_ = S(PBM_GETPOS, 0, 0); printf("SETPOS 500: old %ld now %ld\n", a_, b_); }
    { long a_ = S(PBM_SETPOS, -5, 0); long b_ = S(PBM_GETPOS, 0, 0); printf("SETPOS -5: old %ld now %ld\n", a_, b_); }
    { long a_ = S(PBM_DELTAPOS, 1000, 0); long b_ = S(PBM_GETPOS, 0, 0); printf("DELTAPOS 1000: old %ld now %ld\n", a_, b_); }
    long r = S(PBM_SETRANGE, 0, MAKELPARAM(10, 50));
    printf("SETRANGE 10-50: old %ld-%ld, pos now %ld\n", (long)LOWORD(r), (long)HIWORD(r), S(PBM_GETPOS, 0, 0)); /* r taken before */
    PBRANGE pr = {0, 0};
    { long a_ = S(PBM_GETRANGE, TRUE, &pr), b_ = S(PBM_GETRANGE, FALSE, 0); printf("GETRANGE: low %ld high %ld, struct %d-%d\n", a_, b_, pr.iLow, pr.iHigh); }
    r = S(PBM_SETRANGE32, 0, 200000);
    printf("SETRANGE32 0-200000: old %ld-%ld, high %ld\n", (long)LOWORD(r), (long)HIWORD(r), S(PBM_GETRANGE, FALSE, 0));
    { long a_ = S(PBM_SETPOS, 150000, 0); long b_ = S(PBM_GETPOS, 0, 0); printf("SETPOS 150000: old %ld now %ld\n", a_, b_); }
    { long a_ = S(PBM_SETBARCOLOR, 0, RGB(0, 128, 0)), b_ = S(PBM_SETBARCOLOR, 0, RGB(0, 128, 0)); printf("SETBARCOLOR: old %lx, again %lx\n", a_, b_); }
    { long a_ = S(PBM_SETBKCOLOR, 0, RGB(255, 255, 255)), b_ = S(PBM_SETBKCOLOR, 0, RGB(255, 255, 255)); printf("SETBKCOLOR: old %lx, again %lx\n", a_, b_); }
    UpdateWindow(top);
    DestroyWindow(top);
    return 0;
}
