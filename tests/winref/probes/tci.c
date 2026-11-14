/* TranslateCharsetInfo, which installers ask for the dialog font's character set
 * (InstallShield 12's setup launcher, マスクド上海's MShanghai_trial080111.exe): each charset,
 * code page and font-signature bit, the locales of the common languages, unknown values,
 * and GetTextCharsetInfo's FONTSIGNATURE (the code page bit is in fsCsb[0]).
 * libs: -lgdi32 */
#include "../probe.h"

static void show(const char *what, DWORD src, BOOL r, CHARSETINFO *ci) {
    printf("%s %lu = %d", what, src, r);
    if (r) printf(" charset %u acp %u usb %lx %lx %lx %lx csb %lx %lx", ci->ciCharset, ci->ciACP,
                  ci->fs.fsUsb[0], ci->fs.fsUsb[1], ci->fs.fsUsb[2], ci->fs.fsUsb[3], ci->fs.fsCsb[0], ci->fs.fsCsb[1]);
    printf("\n");
}

int main(void) {
    static const DWORD charsets[] = { 0, 1, 2, 77, 128, 129, 130, 134, 136, 161, 162, 163, 177, 178, 186, 204, 222, 238, 255 };
    static const DWORD cps[] = { 437, 874, 932, 936, 949, 950, 1250, 1251, 1252, 1253, 1254, 1255, 1256, 1257, 1258, 1361, 42, 65001 };
    static const DWORD lcids[] = { 0x0409, 0x0411, 0x0412, 0x0404, 0x0804, 0x0c04, 0x0419, 0x0405, 0x0408, 0x041f, 0x040d, 0x0401, 0x0425, 0x042a, 0x041e, 0x0407 };
    CHARSETINFO ci;
    for (int i = 0; i < (int)(sizeof charsets / sizeof *charsets); i++) {
        memset(&ci, 0xcc, sizeof ci);
        BOOL r = TranslateCharsetInfo((DWORD *)(UINT_PTR)charsets[i], &ci, TCI_SRCCHARSET);
        show("charset", charsets[i], r, &ci);
    }
    for (int i = 0; i < (int)(sizeof cps / sizeof *cps); i++) {
        memset(&ci, 0xcc, sizeof ci);
        BOOL r = TranslateCharsetInfo((DWORD *)(UINT_PTR)cps[i], &ci, TCI_SRCCODEPAGE);
        show("codepage", cps[i], r, &ci);
    }
    for (int b = 0; b < 32; b++) {
        DWORD sig[2] = { 1u << b, 0 };
        memset(&ci, 0xcc, sizeof ci);
        BOOL r = TranslateCharsetInfo(sig, &ci, TCI_SRCFONTSIG);
        show("fontsig bit", b, r, &ci);
    }
    {
        DWORD sig[2] = { (1u << 17) | (1u << 0), 0 };
        memset(&ci, 0xcc, sizeof ci);
        show("fontsig 17|0", sig[0], TranslateCharsetInfo(sig, &ci, TCI_SRCFONTSIG), &ci);
    }
    for (int i = 0; i < (int)(sizeof lcids / sizeof *lcids); i++) {
        memset(&ci, 0xcc, sizeof ci);
        BOOL r = TranslateCharsetInfo((DWORD *)(UINT_PTR)lcids[i], &ci, TCI_SRCLOCALE);
        show("locale", lcids[i], r, &ci);
    }
    memset(&ci, 0xcc, sizeof ci);
    show("flags 7", 128, TranslateCharsetInfo((DWORD *)128, &ci, 7), &ci);
    return 0;
}
