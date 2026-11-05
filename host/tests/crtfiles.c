#define __USE_MINGW_ANSI_STDIO 0
#include <stdio.h>
#include <errno.h>
#include <windows.h>
static void create(const char *name, const char *text) {
    FILE *f = fopen(name, "wb");
    if (!f) ExitProcess(1);
    fwrite(text, 1, 3, f); fclose(f);
}
int main(void) {
    DeleteFileA("crt-old.tmp"); DeleteFileA("crt-new.tmp");
    create("crt-old.tmp", "abc");
    int r = rename("crt-old.tmp", "crt-new.tmp");
    printf("rename %d %d\n", r, GetFileAttributesA("crt-old.tmp") == INVALID_FILE_ATTRIBUTES);
    FILE *f = fopen("crt-new.tmp", "rb");
    if (!f) return 1;
    char b[4] = {0}; fread(b, 1, 3, f); fclose(f);
    printf("content %s\n", b);
    create("crt-old.tmp", "xyz");
    errno = 0; r = rename("crt-old.tmp", "crt-new.tmp");
    printf("exists %d %d\n", r != 0, errno == EACCES);
    r = remove("crt-old.tmp");
    printf("remove %d %d\n", r, GetFileAttributesA("crt-old.tmp") == INVALID_FILE_ATTRIBUTES);
    errno = 0; r = remove("crt-old.tmp");
    printf("missing %d %d\n", r != 0, errno == ENOENT);
    remove("crt-new.tmp");
    return 0;
}
