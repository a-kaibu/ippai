/* EnumDisplaySettings lists the current mode among the numbered ones.
 * SiglusEngine looks the current size up in that list to know the desktop: when
 * sake's list lacked the 1920x1080 screen it took 640x480 and opened 牝アイドル 体験版's 1024x768
 * window at half size. (Sizes differ per machine: only these facts are printed.) */
#include "../probe.h"

int main(void) {
    DEVMODEA cur = {0};
    cur.dmSize = sizeof cur;
    BOOL r = EnumDisplaySettingsA(NULL, ENUM_CURRENT_SETTINGS, &cur);
    printf("current: %d\n", r != 0);
    int listed = 0, n = 0;
    for (DWORD i = 0; i < 4096; i++) {
        DEVMODEA m = {0};
        m.dmSize = sizeof m;
        if (!EnumDisplaySettingsA(NULL, i, &m)) break;
        n++;
        if (m.dmPelsWidth == cur.dmPelsWidth && m.dmPelsHeight == cur.dmPelsHeight) listed = 1;
    }
    printf("modes: %d, current listed %d\n", n > 0, listed);
    return 0;
}
