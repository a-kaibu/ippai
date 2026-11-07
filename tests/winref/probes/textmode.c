/* (printf's arguments are evaluated right to left: calls with effects go before it)
 * stdio's text mode (msvcrt): what "r" / "w" / "rt" streams turn CR LF, lone CR, LF and Ctrl+Z
 * into, as fgetc, fgets, fread, ftell, fseek and fwrite see them, against "rb" / "wb". sake read
 * every stream as binary until 2026-11-07 (winref stdioflag's fgets after fseek). Works in
 * "wr-textmode.bin". libs: */
#include "../probe.h"

static const char *NAME = "wr-textmode.bin";

static void write_raw(const char *b, size_t n) {
    FILE *w = fopen(NAME, "wb");
    fwrite(b, 1, n, w);
    fclose(w);
}
static void hexdump(const char *label, const unsigned char *b, size_t n) {
    printf("%s %u:", label, (unsigned)n);
    for (size_t i = 0; i < n; i++) printf(" %02x", b[i]);
    printf("\n");
}
static void raw_back(const char *label) {
    unsigned char b[64];
    FILE *r = fopen(NAME, "rb");
    size_t n = fread(b, 1, sizeof b, r);
    fclose(r);
    hexdump(label, b, n);
}

int main(void) {
    static const char data[] = "a\r\nb\rc\n\r\r\nd\x1a" "after";
    write_raw(data, sizeof data - 1);
    const char *modes[] = { "r", "rt", "rb" };
    for (int m = 0; m < 3; m++) {
        FILE *f = fopen(NAME, modes[m]);
        unsigned char b[64];
        int n = 0, c;
        while ((c = fgetc(f)) != EOF && n < 64) b[n++] = (unsigned char)c;
        char l[32];
        sprintf(l, "fgetc %s", modes[m]);
        hexdump(l, b, n);
        printf("  then feof %d ftell %ld\n", feof(f) != 0, ftell(f));
        rewind(f);
        size_t k = fread(b, 1, sizeof b, f);
        sprintf(l, "fread %s", modes[m]);
        hexdump(l, b, k);
        rewind(f);
        /* fread in small pieces: a CR LF split across two calls */
        size_t total = 0;
        for (int i = 0; i < 20; i++) total += fread(b + total, 1, 1, f);
        sprintf(l, "fread by 1 %s", modes[m]);
        hexdump(l, b, total);
        rewind(f);
        char line[16];
        printf("fgets %s:", modes[m]);
        while (fgets(line, sizeof line, f)) {
            printf(" [");
            for (char *p = line; *p; p++) printf(*p == '\n' ? "\\n" : *p == '\r' ? "\\r" : "%c", *p);
            printf("] at %ld", ftell(f));
        }
        printf("\n");
        /* fseek to the LF of the first CR LF, then read */
        fseek(f, 2, SEEK_SET);
        c = fgetc(f);
        printf("after fseek 2 %s: fgetc %d ftell %ld\n", modes[m], c, ftell(f));
        fseek(f, 1, SEEK_SET);
        c = fgetc(f);
        printf("after fseek 1 %s: fgetc %d ftell %ld\n", modes[m], c, ftell(f));
        fclose(f);
    }

    /* writing: LF becomes CR LF in "w", a CR stays, "wb" writes bytes as they are */
    const char *wmodes[] = { "w", "wt", "wb" };
    for (int m = 0; m < 3; m++) {
        FILE *w = fopen(NAME, wmodes[m]);
        fputs("x\ny\r\n", w);
        fputc('\n', w);
        size_t k = fwrite("p\nq", 1, 3, w);
        fprintf(w, "%d\n", 7);
        long t = ftell(w);
        fclose(w);
        char l[32];
        sprintf(l, "written %s", wmodes[m]);
        raw_back(l);
        printf("  fwrite returned %u ftell %ld\n", (unsigned)k, t);
    }
    remove(NAME);
    return 0;
}
