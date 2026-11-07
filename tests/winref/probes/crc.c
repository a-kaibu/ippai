/* SSE4.2's CRC32 (CRC-32C) on bytes, words, dwords (and qwords on x86-64) from chosen and
 * running starting values, and POPCNT with its flags, as engines' hashing uses them (Godot's
 * x64 builds). The CPU, not Windows, answers: sake's interpreter and translator must agree. */
#include "../probe.h"

__attribute__((target("sse4.2,popcnt"))) static void run(void) {
    static const unsigned char data[] = "123456789 the quick brown fox \xff\x00\x80\x7f";
    unsigned c = 0xffffffff;
    for (unsigned i = 0; i < 9; i++) c = __builtin_ia32_crc32qi(c, data[i]);
    printf("check %08x\n", c ^ 0xffffffff);
    unsigned starts[] = { 0, 0xffffffff, 0x12345678, 0x80000000 };
    for (unsigned s = 0; s < 4; s++) {
        unsigned b = starts[s], w = starts[s], d = starts[s];
        for (unsigned i = 0; i + 4 <= sizeof data; i += 4) {
            unsigned v;
            memcpy(&v, data + i, 4);
            b = __builtin_ia32_crc32qi(b, (unsigned char)v);
            w = __builtin_ia32_crc32hi(w, (unsigned short)v);
            d = __builtin_ia32_crc32si(d, v);
        }
        printf("start %08x byte %08x word %08x dword %08x", starts[s], b, w, d);
#ifdef _WIN64
        unsigned long long q = starts[s];
        for (unsigned i = 0; i + 8 <= sizeof data; i += 8) {
            unsigned long long v;
            memcpy(&v, data + i, 8);
            q = __builtin_ia32_crc32di(q, v);
        }
        printf(" qword %08llx", q);
#endif
        printf("\n");
    }
    unsigned vals[] = { 0, 1, 0xffffffff, 0x80000001, 0x55555555, 0x00f0f000 };
    for (unsigned i = 0; i < 6; i++) {
        unsigned n;
        unsigned char z;
        __asm__ volatile("popcnt %2, %0; setz %1" : "=r"(n), "=q"(z) : "r"(vals[i]) : "cc");
        printf("popcnt %08x %u zf %u\n", vals[i], n, z);
    }
}

int main(void) {
    run();
    return 0;
}
