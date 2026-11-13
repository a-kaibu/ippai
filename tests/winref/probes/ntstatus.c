/* ntdll's RtlNtStatusToDosError, which MSVC programs find by GetProcAddress (the Edicius trial
 * quit at start without it): the codes programs meet (file, memory, loader, sync, pipes),
 * facility Win32 / debugger codes (the low word), customer codes (unchanged), 0xd... (as 0xc...)
 * and codes with no Win32 error (ERROR_MR_MID_NOT_FOUND). sake: nt::dos_error. */
#include "../probe.h"

typedef ULONG (WINAPI *F)(LONG);

int main(void) {
    HMODULE nt = GetModuleHandleA("ntdll.dll");
    F f = (F)GetProcAddress(nt, "RtlNtStatusToDosError");
    F g = (F)GetProcAddress(nt, "RtlNtStatusToDosErrorNoTeb");
    printf("exports %d %d\n", f != NULL, g != NULL);
    if (!f || !g) return 1;
    static const ULONG codes[] = {
        0, 0x102, 0x103, 0x40000000, 0x80000005, 0x80000006, 0x8000001a,
        0xc0000001, 0xc0000002, 0xc0000003, 0xc0000004, 0xc0000005, 0xc0000008, 0xc000000d, 0xc000000f,
        0xc0000010, 0xc0000011, 0xc0000017, 0xc0000022, 0xc0000023, 0xc0000024, 0xc0000033, 0xc0000034,
        0xc0000035, 0xc000003a, 0xc000003b, 0xc0000043, 0xc0000046, 0xc0000047, 0xc0000056, 0xc0000061,
        0xc000007a, 0xc000007f, 0xc000009a, 0xc00000ba, 0xc00000bb, 0xc0000101, 0xc0000103, 0xc0000120,
        0xc0000135, 0xc0000139, 0xc000014b, 0xc00000b5, 0xc00000d9, 0xc00000ac, 0xc00000ae, 0xc00000b0,
        0xc00000b1, 0xc00000b2, 0xc00000b3, 0xc00000ab, 0xc00000ad, 0xc0000225,
        0x80070005, 0xc0070002, 0xc0010003, 0x8007ffff, 0xd0000034, 0xd0000017,
        0xe0001234, 0x20000001, 0x6000abcd, 0xa0000000,
    };
    for (unsigned i = 0; i < sizeof codes / sizeof *codes; i++) {
        ULONG a = f((LONG)codes[i]), b = g((LONG)codes[i]);
        printf("%08lx -> %lu%s\n", codes[i], a, a == b ? "" : " (NoTeb differs)");
    }
    return 0;
}
