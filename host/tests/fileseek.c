#define __USE_MINGW_ANSI_STDIO 0
#include <stdio.h>
#include <errno.h>
#include <windows.h>
int main(void) {
    FILE *f = fopen("seek-test.tmp", "w+b");
    if (!f) return 1;
    fwrite("abc", 1, 3, f);
    rewind(f);
    printf("first %d\n", fgetc(f));
    errno = 0;
    int r = fseek(f, -1, SEEK_SET);
    printf("negative %d %d %ld\n", r != 0, errno == EINVAL, ftell(f));
    errno = 0;
    r = fseek(f, 0, 99);
    printf("origin %d %d %ld\n", r != 0, errno == EINVAL, ftell(f));
    fseek(f, 0, SEEK_END);
    fgetc(f);
    printf("eof %d\n", feof(f) != 0);
    fseek(f, -1, SEEK_SET);
    printf("failed-eof %d\n", feof(f) != 0);
    fseek(f, 0, SEEK_SET);
    printf("reset %d %ld\n", feof(f) != 0, ftell(f));
    fgetc(f);
    ungetc('z', f);
    fseek(f, 0, SEEK_CUR);
    long at = ftell(f);
    printf("pushback %ld %d\n", at, fgetc(f));
    fclose(f);
    DeleteFileA("seek-test.tmp");
    return 0;
}
