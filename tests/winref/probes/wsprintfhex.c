/* wsprintf's "#" with x and X: where the 0x goes and what the width counts. InstallShield 12's
 * setup launcher (マスクド上海) names its strings file wsprintf("%#04x", 0x411) + ".ini" and
 * extracts 0x0411.ini (the C runtime's printf gives 0x411). Zero padding, left and right
 * alignment, no width, zero, and the W function.
 * libs: -luser32 */
#include "../probe.h"

int main(void) {
    static const char *fmts[] = { "%#04x", "%#04X", "%#08x", "%#-8x|", "%#8x|", "%#x", "%#2x", "%#010lx", "%04x", "%#.6x", "%#06.3x" };
    static const unsigned vals[] = { 0x411, 0, 0xabcdef, 7 };
    char b[256];
    WCHAR w[256], wf[32];
    for (int i = 0; i < (int)(sizeof fmts / sizeof *fmts); i++)
        for (int k = 0; k < (int)(sizeof vals / sizeof *vals); k++) {
            int n = wsprintfA(b, fmts[i], vals[k]);
            MultiByteToWideChar(CP_ACP, 0, fmts[i], -1, wf, 32);
            int nw = wsprintfW(w, wf, vals[k]);
            printf("[%s] %x -> %d [%s] W %d [%ls]\n", fmts[i], vals[k], n, b, nw, w);
        }
    return 0;
}
