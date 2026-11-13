/* Where imported functions live, as programs that check their environment see it: every entry
 * of this program's import table (and every export of kernel32 by GetProcAddress) is committed
 * code of a DLL image (VirtualQuery: MEM_COMMIT, MEM_IMAGE), kernel32!GetModuleHandleA's image is
 * kernel32 itself, and kernel32's file name is the system directory's kernel32.dll.
 * KiriKiri's signed-plugin check (lavender.tpm of 光輪の町、ラベンダーの少女, 2010) does exactly
 * the kernel32 part and refuses to start otherwise ("Authenticatoin failed : unrecognized
 * kernel32 module. / NC"). The expected lines are Windows' answers. x86 (sake's x86-64 system
 * DLLs have no export directory yet; the rest matched there when tried).
 * Also ImageHlp's ImageLoad / MapAndLoad of this program's own file (lavender.tpm next walks the
 * game exe's sections that way).
 * libs: -nostdlib -luser32 -lkernel32 -limagehlp */
#include <windows.h>
#include <imagehlp.h>

static int memcmp_(const void *a, const void *b, int n)
{
    const BYTE *x = a, *y = b;
    for (int i = 0; i < n; i++)
        if (x[i] != y[i]) return x[i] - y[i];
    return 0;
}

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

/* committed executable code of a DLL image (MEM_COMMIT, MEM_IMAGE; the image may be another
 * DLL's: Windows' kernel32 forwards many exports to kernelbase and ntdll) */
static int in_image(const void *p)
{
    MEMORY_BASIC_INFORMATION mbi;
    if (VirtualQuery(p, &mbi, sizeof mbi) != sizeof mbi) return 0;
    return mbi.State == MEM_COMMIT && mbi.Type == MEM_IMAGE &&
           (mbi.Protect & (PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY));
}

static void imports(void)
{
    BYTE *base = (BYTE *)GetModuleHandleA(NULL);
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(base + ((IMAGE_DOS_HEADER *)base)->e_lfanew);
    IMAGE_DATA_DIRECTORY *d = &nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    IMAGE_IMPORT_DESCRIPTOR *imp = (IMAGE_IMPORT_DESCRIPTOR *)(base + d->VirtualAddress);
    for (; imp->Name; imp++) {
        const char *dll = (const char *)(base + imp->Name);
        IMAGE_THUNK_DATA *iat = (IMAGE_THUNK_DATA *)(base + imp->FirstThunk);
        int n = 0, ok = 0;
        for (; iat->u1.Function; iat++, n++)
            ok += in_image((void *)iat->u1.Function);
        CharLowerA((char *)dll);
        out("imports %s: %d of %d in the image\n", dll, ok, n);
    }
}

/* every named export of a DLL (forwarders excluded: they live in another DLL) */
static void exports(const char *dll)
{
    HMODULE m = GetModuleHandleA(dll);
    BYTE *base = (BYTE *)m;
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(base + ((IMAGE_DOS_HEADER *)base)->e_lfanew);
    IMAGE_DATA_DIRECTORY *d = &nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXPORT];
    IMAGE_EXPORT_DIRECTORY *e = (IMAGE_EXPORT_DIRECTORY *)(base + d->VirtualAddress);
    DWORD *names = (DWORD *)(base + e->AddressOfNames);
    int bad = 0, seen = 0;
    for (DWORD i = 0; i < e->NumberOfNames; i++) {
        FARPROC f = GetProcAddress(m, (const char *)(base + names[i]));
        if (!f) continue;
        if ((BYTE *)f >= (BYTE *)e && (BYTE *)f < (BYTE *)e + d->Size) continue;
        seen++;
        bad += !in_image(f);
    }
    out("exports %s: %d not in the image, some seen %d\n", dll, bad, seen > 100);
}

/* lavender.tpm's check, step by step */
static void kirikiri(void)
{
    HMODULE k = GetModuleHandleA("kernel32.dll");
    MEMORY_BASIC_INFORMATION mbi = {0};
    SIZE_T r = VirtualQuery((void *)GetModuleHandleA, &mbi, sizeof mbi);
    out("VirtualQuery(GetModuleHandleA) %d state %x type %x base is kernel32 %d\n", r == sizeof mbi,
        (unsigned)mbi.State, (unsigned)mbi.Type, mbi.AllocationBase == (void *)k);
    char sys[MAX_PATH], file[MAX_PATH];
    UINT n = GetSystemDirectoryA(sys, MAX_PATH);
    lstrcatA(sys, "\\kernel32.dll");
    DWORD f = GetModuleFileNameA(k, file, MAX_PATH);
    out("system directory %d file %d same %d\n", n > 0, f > 0, lstrcmpiA(sys, file) == 0);
}

static void loaded(const char *what, LOADED_IMAGE *li, IMAGE_NT_HEADERS *self)
{
    IMAGE_NT_HEADERS *nt = li->FileHeader;
    BYTE *view = li->MappedAddress;
    out("%s: MZ %d PE %d header in view %d sections %d same %d table %d first %d size %d chars %d readonly %d\n", what,
        view[0] == 'M' && view[1] == 'Z', nt->Signature == IMAGE_NT_SIGNATURE,
        (BYTE *)nt == view + ((IMAGE_DOS_HEADER *)view)->e_lfanew,
        (int)li->NumberOfSections, li->NumberOfSections == self->FileHeader.NumberOfSections,
        (BYTE *)li->Sections == (BYTE *)&nt->OptionalHeader + nt->FileHeader.SizeOfOptionalHeader,
        li->LastRvaSection == li->Sections,
        li->SizeOfImage == self->OptionalHeader.SizeOfImage,
        li->Characteristics == self->FileHeader.Characteristics, li->fReadOnly);
    /* the file as it is, not laid out as an image: the first section's raw data at its file offset */
    out("%s: section name %d raw data at file offset %d\n", what,
        memcmp_(li->Sections[0].Name, IMAGE_FIRST_SECTION(self)->Name, 8) == 0,
        memcmp_(view + li->Sections[0].PointerToRawData, (BYTE *)GetModuleHandleA(NULL) + li->Sections[0].VirtualAddress, 16) == 0);
}

static void imageload(void)
{
    char path[MAX_PATH];
    GetModuleFileNameA(NULL, path, MAX_PATH);
    BYTE *base = (BYTE *)GetModuleHandleA(NULL);
    IMAGE_NT_HEADERS *self = (IMAGE_NT_HEADERS *)(base + ((IMAGE_DOS_HEADER *)base)->e_lfanew);
    LOADED_IMAGE *li = ImageLoad(path, NULL);
    out("ImageLoad self %d\n", li != NULL);
    if (li) {
        loaded("ImageLoad", li, self);
        out("module name %d\n", lstrcmpiA(li->ModuleName, path) == 0);
        out("ImageUnload %d\n", ImageUnload(li));
    }
    SetLastError(0);
    li = ImageLoad("no such file.exe", NULL);
    out("ImageLoad missing %d error %d\n", li != NULL, (int)GetLastError());
    /* the name without its folder and extension: found in the exe's folder, ".exe" added */
    char *name = path + lstrlenA(path);
    while (name > path && name[-1] != '\\') name--;
    char dir[MAX_PATH];
    lstrcpynA(dir, path, (int)(name - path));
    char *dot = name + lstrlenA(name) - 4;
    *dot = 0;
    LOADED_IMAGE m;
    BOOL r = MapAndLoad(name, dir, &m, FALSE, FALSE);
    out("MapAndLoad by name %d\n", r);
    if (r) {
        loaded("MapAndLoad", &m, self);
        out("UnMapAndLoad %d\n", UnMapAndLoad(&m));
    }
}

void mainCRTStartup(void)
{
    kirikiri();
    imageload();
    imports();
    exports("kernel32.dll");
    exports("user32.dll");
    ExitProcess(0);
}
