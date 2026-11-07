/* msvcrt's FILE as programs built with mingw (or old MSVC headers) read it straight: their
 * feof / ferror / fileno macros look at FILE._flag and FILE._file. umineko's PONScripter reads
 * pns.cfg with fgetc until `fp->_flag & _IOEOF`. Here: _file next to _fileno(), the EOF bit
 * as fgetc, fread, fgets reach the end and as fseek, rewind, ungetc, clearerr clear it, the
 * mode bits of "r" and "w" streams, the standard streams' descriptors. Works in "wr-stdioflag".
 * libs: */
#include "../probe.h"

static void flags(const char *l, FILE *f) {
    /* (_IOREAD, _IOWRT, _IOEOF, _IOERR: the buffering bits vary with how much was read) */
    printf("%s: eof bit %d feof %d err bit %d file==fileno %d\n", l, (f->_flag & _IOEOF) != 0, feof(f) != 0, (f->_flag & _IOERR) != 0, f->_file == _fileno(f));
}

int main(void) {
    FILE *w = fopen("wr-stdioflag.txt", "w");
    if (!w) return 1;
    printf("w mode bits read %d write %d\n", (w->_flag & _IOREAD) != 0, (w->_flag & _IOWRT) != 0);
    flags("w", w);
    fputs("ab\nc", w);
    fclose(w);

    FILE *f = fopen("wr-stdioflag.txt", "r");
    printf("r mode bits read %d write %d\n", (f->_flag & _IOREAD) != 0, (f->_flag & _IOWRT) != 0);
    flags("open", f);
    for (int i = 0; i < 6; i++) {
        int c = fgetc(f);
        char l[32];
        sprintf(l, "fgetc %d -> %d", i, c);
        flags(l, f);
    }
    /* the loop PONScripter runs over pns.cfg: count lines until the EOF bit */
    rewind(f);
    flags("rewind", f);
    int lines = 1, n = 0;
    while (!(f->_flag & _IOEOF) && n < 100) {
        if (fgetc(f) == '\n') lines++;
        n++;
    }
    printf("loop read %d lines %d\n", n, lines);
    fseek(f, 0, SEEK_SET);
    flags("fseek", f);
    char b[16];
    printf("fread %u\n", (unsigned)fread(b, 1, sizeof b, f));
    flags("fread past end", f);
    printf("ungetc %d\n", ungetc('x', f));
    flags("ungetc", f);
    printf("fgetc %d\n", fgetc(f));
    printf("fgetc %d\n", fgetc(f));
    flags("end again", f);
    clearerr(f);
    flags("clearerr", f);
    fseek(f, 3, SEEK_SET);
    printf("fgets %s\n", fgets(b, sizeof b, f) ? b : "(null)");
    flags("fgets to end", f);
    printf("fgets again %s\n", fgets(b, sizeof b, f) ? b : "(null)");
    flags("fgets at end", f);
    fclose(f);
    remove("wr-stdioflag.txt");
    printf("std files %d %d\n", stdout->_file, stderr->_file);
    return 0;
}
