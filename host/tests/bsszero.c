/* A section whose PointerToRawData is 0 has no bytes in the file: the loader maps it as zeros,
 * whatever its SizeOfRawData says. The Watcom linker writes .bss that way (raw size 0x1800, file
 * pointer 0, VirtualSize 0); sake copied the file's first 0x1800 bytes (its headers) there, so
 * the Watcom runtime's "processes attached" counter in .bss was not 0 at the first attach and
 * IPF33.DLL / ADM41.DLL of Crazies (ダンシング・クレイジーズ体験版) said "This Dynamic Link Library
 * is already in use by" (#207). The DLL is made here: headers, a .data section with bytes in the
 * file and a .bss like Watcom's. No entry point, no imports. x86 and x86-64; the expected
 * lines are Windows' answers.
 * libs: -nostdlib -lkernel32 -luser32 */
#include <windows.h>

static void out(const char *f, ...)
{
    char b[512];
    DWORD n;
    va_list a;
    va_start(a, f);
    n = wvsprintfA(b, f, a);
    va_end(a);
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), b, n, &n, NULL);
}

static unsigned char img[0x400];
static void w16(int at, unsigned v) { img[at] = v; img[at + 1] = v >> 8; }
static void w32(int at, unsigned v) { w16(at, v & 0xffff); w16(at + 2, v >> 16); }

static void section(int at, const char *name, unsigned vsize, unsigned va, unsigned raw, unsigned ptr, unsigned ch)
{
    for (int i = 0; i < 8 && name[i]; i++) img[at + i] = name[i];
    w32(at + 8, vsize);
    w32(at + 12, va);
    w32(at + 16, raw);
    w32(at + 20, ptr);
    w32(at + 36, ch);
}

/* the image: headers 0..0x200, .data's bytes 0x200..0x400 */
static void build(void)
{
    img[0] = 'M';
    img[1] = 'Z';
    w32(0x3c, 0x40);
    w32(0x40, 0x4550);
#ifdef _WIN64
    const int opt = 0x58, optsize = 0xf0, dirs = opt + 112;
    w16(0x44, 0x8664);
    w16(0x54, optsize);
    w16(0x56, 0x2022);
    w16(opt, 0x20b);
    w32(opt + 24, 0x12340000); /* ImageBase (low dword) */
    w32(opt + 72, 0x100000);   /* stack reserve */
    w32(opt + 80, 0x1000);
    w32(opt + 88, 0x100000);   /* heap reserve */
    w32(opt + 96, 0x1000);
    w32(opt + 108, 16);
#else
    const int opt = 0x58, optsize = 0xe0, dirs = opt + 96;
    w16(0x44, 0x14c);
    w16(0x54, optsize);
    w16(0x56, 0x2102);
    w16(opt, 0x10b);
    w32(opt + 28, 0x12340000);
    w32(opt + 72, 0x100000);
    w32(opt + 76, 0x1000);
    w32(opt + 80, 0x100000);
    w32(opt + 84, 0x1000);
    w32(opt + 92, 16);
#endif
    (void)dirs;
    w16(0x46, 2);
    w32(opt + 32, 0x1000);  /* SectionAlignment */
    w32(opt + 36, 0x200);   /* FileAlignment */
    w16(opt + 40, 4);       /* OS 4.0 */
    w16(opt + 48, 4);       /* subsystem 4.0 */
    w32(opt + 56, 0x3000);  /* SizeOfImage */
    w32(opt + 60, 0x200);   /* SizeOfHeaders */
    w16(opt + 68, 2);       /* GUI */
    int s = opt + optsize;
    section(s, ".data", 0x200, 0x1000, 0x200, 0x200, 0xc0000040);
    section(s + 40, ".bss", 0, 0x2000, 0x200, 0, 0xc0000080);
    for (int i = 0; i < 0x200; i++) img[0x200 + i] = 0x40 + i % 26;
}

/* how many bytes from p on are 0, up to n */
static int zeros(const unsigned char *p, int n)
{
    int k = 0;
    while (k < n && p[k] == 0) k++;
    return k;
}

void mainCRTStartup(void)
{
    char dir[MAX_PATH], path[MAX_PATH];
    DWORD n;
    build();
    GetTempPathA(MAX_PATH, dir);
    wsprintfA(path, "%sbsszero%d.dll", dir, (int)GetCurrentProcessId());
    HANDLE f = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    WriteFile(f, img, sizeof img, &n, NULL);
    CloseHandle(f);
    HMODULE m = LoadLibraryA(path);
    out("load %s (%d)\n", m ? "ok" : "failed", m ? 0 : (int)GetLastError());
    if (m) {
        const unsigned char *b = (const unsigned char *)m;
        out("data %c%c%c, last %c\n", b[0x1000], b[0x1001], b[0x1002], b[0x11ff]);
        out("bss zeros %d of 512\n", zeros(b + 0x2000, 0x200));
        out("free %d\n", FreeLibrary(m));
    }
    out("delete %d\n", DeleteFileA(path));
    ExitProcess(0);
}
