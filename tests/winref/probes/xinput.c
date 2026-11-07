/* XInput as games load it (AdvHD imports XINPUTUAP.dll; others xinput1_4 or xinput9_1_0, all
 * three in System32): each loads, and with no controller attached (the runner has none, nor does
 * sake) every user index answers ERROR_DEVICE_NOT_CONNECTED, an index past 3 ERROR_BAD_ARGUMENTS;
 * XInputEnable returns nothing and changes none of that. */
#include "../probe.h"

typedef DWORD(WINAPI *GetState)(DWORD, void *);
typedef DWORD(WINAPI *SetState)(DWORD, void *);
typedef DWORD(WINAPI *GetCaps)(DWORD, DWORD, void *);
typedef void(WINAPI *Enable)(BOOL);

int main(void) {
    const char *dlls[] = { "xinput1_4.dll", "xinput9_1_0.dll", "XINPUTUAP.dll" };
    for (int d = 0; d < 3; d++) {
        HMODULE m = LoadLibraryA(dlls[d]);
        printf("%s: %s\n", dlls[d], m ? "loaded" : "missing");
        if (!m) continue;
        GetState gs = (GetState)GetProcAddress(m, "XInputGetState");
        SetState ss = (SetState)GetProcAddress(m, "XInputSetState");
        GetCaps gc = (GetCaps)GetProcAddress(m, "XInputGetCapabilities");
        Enable en = (Enable)GetProcAddress(m, "XInputEnable");
        printf("  exports: GetState %d SetState %d GetCapabilities %d Enable %d\n", !!gs, !!ss, !!gc, !!en);
        for (int pass = 0; pass < 2; pass++) {
            if (pass && en) en(FALSE);
            for (DWORD i = 0; i < 5; i++) {
                BYTE state[16] = { 0 }, vib[4] = { 0 }, caps[20] = { 0 };
                printf("  %s user %lu: GetState %lu SetState %lu GetCapabilities %lu\n", pass ? "disabled" : "enabled", i,
                       gs ? gs(i, state) : 0xffff, ss ? ss(i, vib) : 0xffff, gc ? gc(i, 0, caps) : 0xffff);
            }
        }
        if (en) en(TRUE);
    }
    return 0;
}
