/* SystemParametersInfo(SPI_GETDEFAULTINPUTLANG): TRUE and the HKL GetKeyboardLayout(0) answers,
 * written pointer-sized (an x64 HKL is 8 bytes), through the A and the W call. Trials check
 * it for 0x411 before they start (DLPC04.EXE: "本製品は日本国内専用です" when it is 0).
 * GetKeyboardType's type (0) and subtype (1) are printed as whether they are the Japanese
 * keyboard's (7, 2) when the layout is Japanese, the 101-key's (4, 0) otherwise. */
#include "../probe.h"

int main(void) {
    HKL kl = GetKeyboardLayout(0);
    int ja = ((ULONG_PTR)kl & 0xffff) == 0x411;
    for (int wide = 0; wide < 2; wide++) {
        HKL v;
        memset(&v, 0xcd, sizeof v);
        BOOL r = wide ? SystemParametersInfoW(SPI_GETDEFAULTINPUTLANG, 0, &v, 0) : SystemParametersInfoA(SPI_GETDEFAULTINPUTLANG, 0, &v, 0);
        printf("SPI_GETDEFAULTINPUTLANG %c: ret %d same %d\n", wide ? 'W' : 'A', r != 0, v == kl);
    }
    int t = GetKeyboardType(0), st = GetKeyboardType(1);
    printf("GetKeyboardType: type %d subtype %d fkeys %d\n", t == (ja ? 7 : 4), st == (ja ? 2 : 0), GetKeyboardType(2));
    return 0;
}
