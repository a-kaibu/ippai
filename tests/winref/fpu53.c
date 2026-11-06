/* Linked into every winref exe: the x87 precision MSVC's C runtime starts programs with (53-bit
 * mantissa; mingw's startup leaves 64), the one games run at and sake models. */
#include <float.h>
static void __attribute__((constructor)) fpu53(void) { _controlfp(_PC_53, _MCW_PC); }
