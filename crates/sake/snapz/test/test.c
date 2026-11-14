/* snapz.c's error paths (no zstd needed): cc -I test -o /tmp/snapz-test test/test.c && /tmp/snapz-test
 * A context zstd could not make (the worker's memory did not grow) is an error, 0 or (size_t)-1,
 * and never handed to zstd; once memory is there again the next call makes it and works. */
#include <assert.h>
#include <stdio.h>
int fake_no_memory, fake_used_null;
#include "../snapz.c"

int main(void) {
    char src[100], dst[200], back[200];
    for (int i = 0; i < 100; i++) src[i] = (char)i;
    fake_no_memory = 1;
    assert(snapz_compress(dst, sizeof dst, src, sizeof src, 3) == 0);
    assert(snapz_decompress(back, sizeof back, dst, 100) == (size_t)-1);
    assert(!fake_used_null);
    fake_no_memory = 0;
    size_t n = snapz_compress(dst, sizeof dst, src, sizeof src, 3);
    assert(n == 100);
    assert(snapz_decompress(back, sizeof back, dst, n) == 100);
    assert(memcmp(back, src, 100) == 0);
    /* (an error from zstd itself stays one) */
    assert(snapz_compress(dst, 10, src, sizeof src, 3) == 0);
    assert(snapz_decompress(back, 10, dst, n) == (size_t)-1);
    puts("snapz: ok");
    return 0;
}
