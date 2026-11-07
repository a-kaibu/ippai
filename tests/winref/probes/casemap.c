/* Case mapping of every BMP code point, as programs upper- and lower-case text: CharUpperBuffW /
 * CharLowerBuffW (user32) and LCMapStringW LCMAP_UPPERCASE / LCMAP_LOWERCASE in Japanese
 * (kernel32). One line per code point that any of them changes (unchanged ones are left out);
 * sake's text::upper16 / lower16 answer all four. */
#include "../probe.h"

int main(void) {
    unsigned changed = 0;
    for (unsigned c = 1; c < 0x10000; c++) {
        WCHAR cu = (WCHAR)c, cl = (WCHAR)c, lu = 0, ll = 0;
        CharUpperBuffW(&cu, 1);
        CharLowerBuffW(&cl, 1);
        WCHAR in = (WCHAR)c;
        int nu = LCMapStringW(0x411, LCMAP_UPPERCASE, &in, 1, &lu, 1);
        int nl = LCMapStringW(0x411, LCMAP_LOWERCASE, &in, 1, &ll, 1);
        if (nu != 1) lu = 0;
        if (nl != 1) ll = 0;
        if (cu == c && cl == c && lu == c && ll == c) continue;
        changed++;
        printf("%04x up %04x lo %04x lcmap up %04x lo %04x\n", c, cu, cl, lu, ll);
    }
    printf("changed %u\n", changed);
    return 0;
}
