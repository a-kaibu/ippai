/* Two DLLs of the same name in different folders, loaded by full path: Windows matches a
 * path-qualified LoadLibrary against the loaded modules by the whole path, so both load, each
 * its own module (Ren'Py 6.99/7 loads renpy\angle\gldraw.pyd and then renpy\gl\gldraw.pyd,
 * #146). The DLLs are written here: no imports, an entry point that returns TRUE, a tag at RVA
 * 0x1010 telling the copies apart, one relocation. Then the same path again, another spelling of it, the bare
 * name (a loaded module's base name), and FreeLibrary of each until the second is unloaded. */
#include "../probe.h"

static void put16(unsigned char *p, unsigned v) { p[0] = v; p[1] = v >> 8; }
static void put32(unsigned char *p, unsigned v) { put16(p, v); put16(p + 2, v >> 16); }

static int write_dll(const char *path, unsigned tag) {
    unsigned char d[0x400] = { 0 };
    int wide = sizeof(void *) == 8;
    d[0] = 'M'; d[1] = 'Z';
    put32(d + 0x3c, 0x40);
    memcpy(d + 0x40, "PE\0\0", 4);
    unsigned char *f = d + 0x44, *o = f + 20;
    put16(f, wide ? 0x8664 : 0x14c);
    put16(f + 2, 1);
    put16(f + 16, wide ? 0xf0 : 0xe0);
    put16(f + 18, 0x2002 | (wide ? 0x20 : 0x100));
    put16(o, wide ? 0x20b : 0x10b);
    put32(o + 4, 0x200);
    put32(o + 16, 0x1000);
    put32(o + 20, 0x1000);
    /* (PE32: BaseOfData, then a 4-byte ImageBase; PE32+: an 8-byte ImageBase there) */
    if (wide) put32(o + 24, 0x10000000);
    else put32(o + 24, 0x1000), put32(o + 28, 0x10000000);
    put32(o + 32, 0x1000);
    put32(o + 36, 0x200);
    put16(o + 40, 5); put16(o + 48, 5);
    put32(o + 56, 0x2000);
    put32(o + 60, 0x200);
    put16(o + 68, 2);
    /* stack and heap reserve / commit, then the number of data directories (all empty) */
    int w = wide ? 8 : 4;
    put32(o + 72, 0x100000); put32(o + 72 + w, 0x1000); put32(o + 72 + 2 * w, 0x100000); put32(o + 72 + 3 * w, 0x1000);
    put32(o + 72 + 4 * w + 4, 16);
    unsigned char *sh = o + (wide ? 0xf0 : 0xe0);
    memcpy(sh, ".text", 5);
    put32(sh + 8, 0x200);
    put32(sh + 12, 0x1000);
    put32(sh + 16, 0x200);
    put32(sh + 20, 0x200);
    put32(sh + 36, 0x60000020);
    /* mov eax, 1; ret 12 (x86) / ret (x64) */
    static const unsigned char x86[] = { 0xb8, 1, 0, 0, 0, 0xc2, 12, 0 }, x64[] = { 0xb8, 1, 0, 0, 0, 0xc3 };
    memcpy(d + 0x200, wide ? x64 : x86, wide ? sizeof x64 : sizeof x86);
    put32(d + 0x210, tag);
    /* at RVA 0x1018 the address of the tag, fixed up by the one relocation (the second copy
     * cannot have the base the first took): a block for page 0x1000, HIGHLOW / DIR64 at 0x18 */
    put32(d + 0x218, 0x10001010);
    put32(d + 0x300, 0x1000);
    put32(d + 0x304, 12);
    put16(d + 0x308, (wide ? 0xa000 : 0x3000) | 0x18);
    unsigned char *dirs = o + (wide ? 112 : 96);
    put32(dirs + 40, 0x1100);
    put32(dirs + 44, 12);
    FILE *fp = fopen(path, "wb");
    if (!fp) return 0;
    fwrite(d, 1, sizeof d, fp);
    fclose(fp);
    return 1;
}

static unsigned tag(HMODULE m) { return m ? *(unsigned *)((char *)m + 0x1010) : 0; }
static int fixed(HMODULE m) { return m && *(char **)((char *)m + 0x1018) == (char *)m + 0x1010; }

int main(void) {
    char cwd[MAX_PATH], a[MAX_PATH + 32], b[MAX_PATH + 32], a2[MAX_PATH + 32];
    GetCurrentDirectoryA(MAX_PATH, cwd);
    CreateDirectoryA("samename_a", NULL);
    CreateDirectoryA("samename_b", NULL);
    snprintf(a, sizeof a, "%s\\samename_a\\twin.dll", cwd);
    snprintf(b, sizeof b, "%s\\samename_b\\twin.dll", cwd);
    snprintf(a2, sizeof a2, "%s\\samename_b\\..\\SAMENAME_A\\TWIN.DLL", cwd);
    if (!write_dll(a, 0xa1) || !write_dll(b, 0xb2)) { puts("skip cannot write the DLLs"); return 0; }
    HMODULE ha = LoadLibraryA(a);
    printf("first %d tag %#x fixed up %d\n", ha != NULL, tag(ha), fixed(ha));
    HMODULE hb = LoadLibraryA(b);
    printf("second %d tag %#x fixed up %d, another module %d\n", hb != NULL, tag(hb), fixed(hb), hb != ha);
    HMODULE hb2 = LoadLibraryExA(b, NULL, LOAD_WITH_ALTERED_SEARCH_PATH);
    printf("second again (altered search path): the second %d\n", hb2 == hb);
    HMODULE ha2 = LoadLibraryA(a2);
    printf("first by another spelling: the first %d\n", ha2 == ha);
    char fn[MAX_PATH];
    GetModuleFileNameA(hb, fn, MAX_PATH);
    printf("second's file name is its path %d\n", lstrcmpiA(fn, b) == 0);
    HMODULE bare = LoadLibraryA("twin.dll");
    printf("bare name: one of them %d\n", bare == ha || bare == hb);
    printf("GetModuleHandle bare: one of them %d\n", GetModuleHandleA("twin.dll") == ha || GetModuleHandleA("twin.dll") == hb);
    printf("GetModuleHandle second's path: the second %d\n", GetModuleHandleA(b) == hb);
    if (bare) FreeLibrary(bare);
    FreeLibrary(ha2);
    FreeLibrary(hb2);
    printf("free first %d, second %d\n", FreeLibrary(ha), FreeLibrary(hb));
    printf("unloaded: second %d\n", GetModuleHandleA(b) == NULL);
    DeleteFileA(a);
    DeleteFileA(b);
    RemoveDirectoryA("samename_a");
    RemoveDirectoryA("samename_b");
    return 0;
}
