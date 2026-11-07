/* C++ name undecoration (undname.rs): dbghelp's UnDecorateSymbolName and msvcrt's __unDName
 * on names of each kind the decoration grammar has (functions, members, operators, data,
 * vtables, RTTI, thunks, templates, back-references, pointers to functions and arrays, x64's
 * __ptr64), with the flags programs pass (complete, name only, no access specifiers, no MS
 * keywords, type only), and names that are not decorated (they come back as they are).
 * libs: -ldbghelp */
#include "../probe.h"
#include <dbghelp.h>

static const char *names[] = {
    "?f@@YAXXZ",
    "?g@ns@@YGHHPBD@Z",
    "?m@C@@QAEXH@Z",
    "?m@C@@QBEXXZ",
    "?m@C@@UEAAXXZ",
    "?m@C@@UEBAXXZ",
    "?m@C@@SAHXZ",
    "?m@C@@KAXXZ",
    "?m@C@@EAEXXZ",
    "?v@@YAXHZZ",
    "?f@@YIX_N_W_J@Z",
    "?f@@YAXMNO_K@Z",
    "??0Foo@@QAE@XZ",
    "??1Foo@@UAE@XZ",
    "??4Foo@@QAEAAV0@ABV0@@Z",
    "??BFoo@@QBEHXZ",
    "??HFoo@@QBE?AV0@ABV0@@Z",
    "??RFoo@@QAEXXZ",
    "??_GFoo@@UAEPAXI@Z",
    "??_EFoo@@UAEPAXI@Z",
    "??_7Foo@@6B@",
    "??_7Foo@@6BBar@@@",
    "??_8Foo@@7B@",
    "??_R4Foo@@6B@",
    "??_R0?AVFoo@@@8",
    "??_R0?AUBar@ns@@@8",
    "??_R1A@?0A@EA@Foo@@8",
    "??_R2Foo@@8",
    "??_R3Foo@@8",
    "??_C@_03KOEHGMDN@abc?$AA@",
    "?f@Bar@Foo@@QAEXPAV12@0@Z",
    "?f@@YAXHPAH0@Z",
    "?f@?$Vec@H$02@@QAEXXZ",
    "?x@@3V?$A@V?$B@H@@@@A",
    "??$max@N@@YANNN@Z",
    "??0?$Box@VFoo@@@@QAE@ABV0@@Z",
    "?n@@3HA",
    "?n@@3HB",
    "?s@C@@2PBDB",
    "?s@C@@0HA",
    "?p@@3PAY01HA",
    "?fp@@3P6AHH@ZA",
    "?fpp@@3PAP6AHH@ZA",
    "?pm@@3PQC@@HQ1@",
    "?f@@YAXQAH@Z",
    "?f@@YAXPAPAD@Z",
    "?f@@YAXAEBV?$basic_string@D@@@Z",
    "?f@@YAP6AXXZXZ",
    "?f@@YAXP6GHPAX@Z@Z",
    "?f@@YAXP8C@@AEXH@Z@Z",
    "?f@@YAXP8C@@BEXXZ@Z",
    "?f@@YAXPAY1BE@BO@H@Z",
    "?f@@YAXW4E@@@Z",
    "?f@@YAXTU@@@Z",
    "?f@?A0x1234abcd@@YAXXZ",
    "?x@?1??f@@YAXXZ@4HA",
    "?m@C@@W7AEXXZ",
    "?m@C@@$4PPPPPPPM@A@AEXXZ",
    "??_9C@@$BA@AE",
    "??$?6D@std@@YAXXZ",
    "?$T@H",
    "?f@@YAX$$QAH@Z",
    "?f@@YAX_Q_S_U@Z",
    "?f@@YAX$$T@Z",
    "?f@@YAXPEIFAH@Z",
    "plain_c_name",
    "_f@4",
    "?bad.name@@YAXXZ",
    "?f@@YAX",
    "",
};

static const DWORD flags[] = { 0, UNDNAME_NAME_ONLY, UNDNAME_NO_ACCESS_SPECIFIERS, UNDNAME_NO_MS_KEYWORDS, UNDNAME_NO_FUNCTION_RETURNS,
                               UNDNAME_NO_LEADING_UNDERSCORES, 0x60, UNDNAME_NO_MEMBER_TYPE };

typedef char *(__cdecl *undname_fn)(char *, const char *, int, void *(__cdecl *)(size_t), void(__cdecl *)(void *), unsigned short);

int main(void) {
    char out[1024];
    for (size_t i = 0; i < sizeof names / sizeof *names; i++) {
        for (size_t k = 0; k < sizeof flags / sizeof *flags; k++) {
            DWORD n = UnDecorateSymbolName(names[i], out, sizeof out, flags[k]);
            printf("%s %#lx: %lu [%s]\n", names[i], flags[k], n, n ? out : "");
        }
    }
    /* a short buffer: cut, the length what was written */
    DWORD n = UnDecorateSymbolName("?f@@YAXXZ", out, 8, 0);
    printf("short: %lu [%s]\n", n, out);

    undname_fn und = (undname_fn)GetProcAddress(LoadLibraryA("msvcrt.dll"), "__unDName");
    printf("__unDName: %d\n", und != NULL);
    if (und) {
        static const char *types[] = { "?AVFoo@ns@@", "?AUBar@@", "PBD", "ABVVec@@", "?AV?$Box@H@@", "H" };
        for (size_t i = 0; i < sizeof types / sizeof *types; i++) {
            char *s = und(NULL, types[i], 0, malloc, free, 0x2800);
            printf("type %s: [%s]\n", types[i], s ? s : "(null)");
            free(s);
        }
        for (size_t i = 0; i < 12; i++) {
            char *s = und(NULL, names[i], 0, malloc, free, 0);
            printf("und %s: [%s]\n", names[i], s ? s : "(null)");
            free(s);
        }
        char *s = und(out, "?f@@YAXXZ", 10, malloc, free, 0);
        printf("und into 10: %d [%s]\n", s == out, out);
        printf("und no alloc: %d\n", und(NULL, "?f@@YAXXZ", 0, NULL, free, 0) == NULL);
    }
    return 0;
}
