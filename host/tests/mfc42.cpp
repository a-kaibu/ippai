// mfc42.dll as a Visual C++ 6 MFC program uses it (#159): its exports by ordinal, thiscall
// members, objects laid out as MFC 4.2's (the program's inline code reads CString's header at
// [p - 12], a CWnd's m_hWnd at +0x20...). CString's reference counting and buffer sizes,
// run-time classes, MFC's message maps, CDialog/CWnd fields and the handle map, a dialog made
// modeless (attached by the creation hook, MFC's window procedure) and one run modal (ended
// by IDOK through the message maps), CWinApp with AfxWinInit, CFile, and CFileException thrown
// as MFC throws it, caught by class. Prints only what any Windows gives (no addresses or
// paths). Built by build-msvc32.sh (x86, msvcrt; no MFC headers or libraries: the members are
// called through pointers GetProcAddress gives by ordinal).
typedef __SIZE_TYPE__ size_t;
extern "C" {
typedef void *HANDLE;
typedef unsigned long DWORD;
__declspec(dllimport) void *__stdcall LoadLibraryA(const char *);
__declspec(dllimport) void *__stdcall GetProcAddress(void *, const char *);
__declspec(dllimport) void *__stdcall GetModuleHandleA(const char *);
__declspec(dllimport) DWORD __stdcall GetCurrentThreadId(void);
__declspec(dllimport) HANDLE __stdcall GetCurrentThread(void);
__declspec(dllimport) int __stdcall DeleteFileA(const char *);
int printf(const char *, ...);
int strcmp(const char *, const char *);
size_t strlen(const char *);
void *memset(void *, int, size_t);
}
// MFC42U (mfc42u.cpp includes this with MFCU): TCHAR is unsigned short, CString UTF-16, the
// exports other ordinals (O(mfc42's, mfc42u's))
#ifdef MFCU
typedef wchar_t TCH; // (16 bits on Windows: MFC42U's unsigned short)
#define T(s) L##s
#define O(a, u) u
#define DLL "mfc42u.dll"
#else
typedef char TCH;
#define T(s) s
#define O(a, u) a
#define DLL "mfc42.dll"
#endif
// TCHAR text printed: ASCII as is, others as \uXXXX (rotating buffers: several in one printf)
static const char *ps(const TCH *s) {
    static char buf[4][512];
    static int k;
    char *b = buf[k++ & 3], *o = b;
    if (!s)
        return "(null)";
    for (; *s && o < b + 500; s++) {
        unsigned c = (unsigned)*s & (sizeof(TCH) == 1 ? 0xff : 0xffff);
        if (c >= 0x20 && c < 0x7f)
            *o++ = (char)c;
        else {
            static const char hex[] = "0123456789abcdef";
            *o++ = '\\';
            *o++ = sizeof(TCH) == 1 ? 'x' : 'u';
            for (int sh = sizeof(TCH) == 1 ? 4 : 12; sh >= 0; sh -= 4)
                *o++ = hex[(c >> sh) & 15];
        }
    }
    *o = 0;
    return b;
}
static size_t tlen(const TCH *s) {
    size_t n = 0;
    while (s && s[n])
        n++;
    return n;
}
// (type_info's vftable: msvcrt.lib has it; nothing here calls through it)
extern "C" const void *const type_info_vftable[2] asm("??_7type_info@@6B@") = {0, 0};

static void *mfc;
static void *fn(int ord) { return GetProcAddress(mfc, (const char *)(size_t)ord); }
static unsigned at(const void *p, int o) { return *(const unsigned *)((const char *)p + o); }

// run-time and exception classes (only their names matter: the catch clauses' types)
class CObject {};
class CException : public CObject {};
class CFileException : public CException {};

typedef void(__thiscall *T0)(void *);
typedef void *(__thiscall *T1)(void *, unsigned);
typedef void *(__thiscall *T2)(void *, unsigned, unsigned);
typedef void *(__thiscall *T3)(void *, unsigned, unsigned, unsigned);
typedef void *(__stdcall *S0)(void);
typedef void *(__stdcall *S1)(unsigned);
typedef void *(__stdcall *S2)(unsigned, unsigned);
typedef void *(__stdcall *S3)(unsigned, unsigned, unsigned);
typedef void *(__stdcall *S4)(unsigned, unsigned, unsigned, unsigned);
typedef void(__cdecl *Fmt)(void *, const TCH *, ...);

struct Str {
    TCH *p;
};
static Str mk(const TCH *s) {
    Str r;
    ((T1)fn(O(537, 538)))(&r, (unsigned)s);
    return r;
}
static void dtor(Str *s) { ((T0)fn(O(800, 800)))(s); }
static void show(const char *what, Str *s) {
    TCH *p = s->p;
    printf("%s \"%s\" refs %d len %u alloc %u\n", what, ps(p), (int)at(p, -12), at(p, -8), at(p, -4));
}

static void strings() {
    Str e;
    ((T0)fn(O(540, 540)))(&e);
    printf("empty refs %d len %u\n", (int)at(e.p, -12), at(e.p, -8));
    Str a = mk(T("hello"));
    show("a", &a);
    Str b;
    ((T1)fn(O(535, 535)))(&b, (unsigned)&a);
    printf("copy shares %d refs %d\n", a.p == b.p, (int)at(a.p, -12));
    ((T1)fn(O(940, 941)))(&b, '!');
    printf("write unshares %d\n", a.p != b.p);
    show("a", &a);
    show("b", &b);
    // (an empty string assigned is the shared empty one again)
    ((T1)fn(O(858, 858)))(&b, (unsigned)&e);
    printf("assigned empty is nil %d\n", b.p == e.p);
    TCH big[700];
    static const int lens[] = {1, 64, 65, 128, 129, 256, 257, 512, 513, 600};
    for (int n : lens) {
        for (int i = 0; i < n; i++)
            big[i] = 'x';
        big[n] = 0;
        ((T1)fn(O(860, 861)))(&b, (unsigned)big);
        printf("len %d alloc %u\n", n, at(b.p, -4));
    }
    // (shrinking keeps the buffer)
    ((T1)fn(O(860, 861)))(&b, (unsigned)T("short"));
    show("b", &b);
    Str t = mk(T("  \t lead and trail \r\n "));
    ((T0)fn(O(6282, 6278)))(&t);
    show("trimleft", &t);
    ((T0)fn(O(6283, 6279)))(&t);
    show("trimright", &t);
    Str sum;
    ((S3)fn(O(924, 925)))((unsigned)&sum, (unsigned)&a, (unsigned)T(" world"));
    show("cs+sz", &sum);
    dtor(&sum);
    ((S3)fn(O(926, 927)))((unsigned)&sum, (unsigned)T("say "), (unsigned)&a);
    show("sz+cs", &sum);
    dtor(&sum);
    ((T1)fn(O(941, 942)))(&a, (unsigned)T(", again"));
    show("+=", &a);
    Str f;
    ((T0)fn(O(540, 540)))(&f);
    ((Fmt)fn(O(2818, 2810)))(&f, T("%d-%s-%04x"), 42, T("x"), 0xbeef);
    show("format", &f);
    Str m;
    ((T3)fn(O(4278, 4273)))(&a, (unsigned)&m, 2, 5);
    show("mid", &m);
    dtor(&m);
    ((T3)fn(O(4278, 4273)))(&a, (unsigned)&m, 40, 5);
    show("mid past end", &m);
    dtor(&m);
    ((T2)fn(O(4129, 4124)))(&a, (unsigned)&m, 3);
    show("left", &m);
    dtor(&m);
    ((T2)fn(O(5710, 5706)))(&a, (unsigned)&m, 5);
    show("right", &m);
    dtor(&m);
    printf("find %d %d\n", (int)(size_t)((T1)fn(O(2764, 2756)))(&a, (unsigned)T("again")), (int)(size_t)((T1)fn(O(2764, 2756)))(&a, (unsigned)T("zz")));
    ((T0)fn(O(2614, 2606)))(&a);
    printf("emptied is nil %d\n", a.p == e.p);
#ifdef MFCU
    // Unicode: the ideographic space trimmed (iswspace), CJK kept; an ANSI string converted
    Str j = mk(L"\u3000 \u65e5\u672c\u8a9e\u3000");
    ((T0)fn(O(6282, 6278)))(&j);
    ((T0)fn(O(6283, 6279)))(&j);
    show("trim cjk", &j);
    Str an;
    ((T1)fn(537))(&an, (unsigned)"ansi");
    show("from ansi", &an);
    ((T1)fn(941))(&an, 0x3042);
    show("+= wchar", &an);
    printf("find wchar %d\n", (int)(size_t)((T1)fn(2755))(&an, 0x3042));
    dtor(&j);
    dtor(&an);
#endif
    Str *all[] = {&a, &b, &e, &t, &f};
    for (Str *s : all)
        dtor(s);
}

static const char *name_of(void *rtc) { return rtc ? *(const char **)rtc : "(none)"; }
static void classes() {
    static const struct { int ord; const char *name; } c[] = {
        {O(1871, 1863), "CObject"}, {O(1799, 1791), "CCmdTarget"}, {O(1948, 1940), "CWinThread"}, {O(1946, 1938), "CWinApp"}, {O(1949, 1941), "CWnd"},
        {O(1816, 1808), "CDialog"}, {O(1833, 1825), "CFile"}, {O(1832, 1824), "CException"}, {O(1835, 1827), "CFileException"},
    };
    for (auto &k : c) {
        void *r = fn(k.ord);
        void *(__stdcall * base)(void) = *(void *(__stdcall **)(void))((char *)r + 16);
        printf("class %s name %s size %#x schema %#x base %s\n", k.name, name_of(r), at(r, 4), at(r, 8) & 0xffff, base ? name_of(base()) : "(none)");
    }
}

struct Entry {
    unsigned msg, code, id, last, sig;
    void *pfn;
};
struct Map {
    const Map *(__stdcall *base)(void);
    const Entry *entries;
};
static void maps() {
    static const struct { int ord; const char *name; } m[] = {{O(4226, 4221), "CCmdTarget"}, {O(4275, 4270), "CWnd"}, {O(4234, 4229), "CDialog"}, {O(4274, 4269), "CWinApp"}};
    const Map *all[4];
    for (int i = 0; i < 4; i++)
        all[i] = (const Map *)fn(m[i].ord);
    for (int i = 0; i < 4; i++) {
        const Map *b = all[i]->base ? all[i]->base() : 0;
        const char *bn = "(none)";
        for (int k = 0; k < 4; k++)
            if (b == all[k])
                bn = m[k].name;
        printf("map %s base %s\n", m[i].name, b && !strcmp(bn, "(none)") ? "(other)" : bn);
    }
    // the entries a dialog's command and initialization go through
    static const struct { int map; unsigned msg, id; const char *what; } want[] = {
        {2, 0x111, 1, "CDialog IDOK"}, {2, 0x111, 2, "CDialog IDCANCEL"}, {2, 0x110, 0, "CDialog WM_INITDIALOG"},
        {2, 0x30, 0, "CDialog WM_SETFONT"}, {1, 0x82, 0, "CWnd WM_NCDESTROY"},
    };
    for (auto &w : want) {
        const Entry *e = all[w.map]->entries;
        while (e->sig && !(e->msg == w.msg && e->code == 0 && e->id == w.id && e->last == w.id))
            e++;
        printf("entry %s sig %u\n", w.what, e->sig);
    }
}

static unsigned char tmpl[64] __attribute__((aligned(4)));
static void *modal;
static void __stdcall tick(void *h, unsigned, unsigned id, unsigned) {
    void *user = LoadLibraryA("user32.dll");
    ((int(__stdcall *)(void *, unsigned))GetProcAddress(user, "KillTimer"))(h, id);
    ((int(__stdcall *)(void *, unsigned, unsigned, unsigned))GetProcAddress(user, "PostMessageA"))((void *)at(modal, 0x20), 0x111, 1, 0);
}

static void windows() {
    void *user = LoadLibraryA("user32.dll");
    void *(__stdcall * desktop)(void) = (void *(__stdcall *)(void))GetProcAddress(user, "GetDesktopWindow");
    long(__stdcall * getlong)(void *, int) = (long(__stdcall *)(void *, int))GetProcAddress(user, "GetWindowLongA");
    int(__stdcall * iswindow)(void *) = (int(__stdcall *)(void *))GetProcAddress(user, "IsWindow");
    void *(__stdcall * settimer)(void *, unsigned, unsigned, void *) = (void *(__stdcall *)(void *, unsigned, unsigned, void *))GetProcAddress(user, "SetTimer");
    void *ms = ((S0)fn(O(1168, 1165)))();
    void *d = desktop();
    void *w1 = ((S1)fn(O(2864, 2859)))((unsigned)d), *w2 = ((S1)fn(O(2864, 2859)))((unsigned)d);
    printf("FromHandle same %d hwnd ok %d permanent %d\n", w1 == w2, at(w1, 0x20) == (unsigned)d, ((S1)fn(O(2867, 2862)))((unsigned)d) != 0);
    unsigned dlg[0x60 / 4];
    ((T2)fn(O(324, 324)))(dlg, 0x66, 0);
    printf("CDialog(0x66) help %#x template %#x hwnd %u refs %u expected %u state %d parent %u\n", dlg[0x40 / 4], dlg[0x44 / 4], dlg[0x20 / 4], dlg[1], dlg[5], dlg[7] == (unsigned)ms, dlg[0x54 / 4]);
    printf("IsKindOf CDialog %d CWnd %d CFile %d\n", (int)(size_t)((T1)fn(O(4083, 4078)))(dlg, (unsigned)fn(O(1816, 1808))), (int)(size_t)((T1)fn(O(4083, 4078)))(dlg, (unsigned)fn(O(1949, 1941))), (int)(size_t)((T1)fn(O(4083, 4078)))(dlg, (unsigned)fn(O(1833, 1825))));
    printf("GetRuntimeClass %s\n", name_of(((void *(__thiscall *)(void *))fn(O(3597, 3592)))(dlg)));
    ((T0)fn(O(641, 641)))(dlg);
    // a modeless dialog from a template in memory: attached while created, MFC's procedure
    unsigned *t = (unsigned *)tmpl;
    t[0] = 0x80c80080; // WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME
    ((unsigned short *)tmpl)[7] = 120;
    ((unsigned short *)tmpl)[8] = 60;
    ((T0)fn(O(326, 326)))(dlg);
    int ok = (int)(size_t)((T3)fn(O(2170, 2162)))(dlg, (unsigned)tmpl, 0, 0);
    void *h = (void *)dlg[0x20 / 4];
#ifdef MFCU
    // (MFC42U's windows are Unicode ones: CreateDialogIndirectParamW, SetWindowLongW)
    int(__stdcall * isuni)(void *) = (int(__stdcall *)(void *))GetProcAddress(user, "IsWindowUnicode");
    printf("unicode window %d\n", isuni(h));
#endif
    printf("CreateIndirect %d attached %d wndproc mfc %d\n", ok, ((S1)fn(O(2867, 2862)))((unsigned)h) == dlg, (void *)getlong(h, -4) == ((S0)fn(O(1153, 1150)))());
    int destroyed = (int)(size_t)((void *(__thiscall *)(void *))fn(O(2446, 2438)))(dlg);
    printf("DestroyWindow %d hwnd %u window gone %d permanent %d\n", destroyed, dlg[0x20 / 4], !iswindow(h), ((S1)fn(O(2867, 2862)))((unsigned)h) != 0);
    ((T0)fn(O(641, 641)))(dlg);
    // modal: IDOK posted by a timer ends it through CDialog's message map (OnOK, EndDialog(IDOK))
    ((T0)fn(O(326, 326)))(dlg);
    ((T3)fn(O(3957, 3952)))(dlg, (unsigned)tmpl, 0, 0);
    modal = dlg;
    settimer(0, 0, 50, (void *)tick);
    int r = (int)(size_t)((void *(__thiscall *)(void *))fn(O(2514, 2506)))(dlg);
    printf("DoModal %d hwnd %u\n", r, dlg[0x20 / 4]);
    ((T0)fn(O(641, 641)))(dlg);
}

static void app() {
    unsigned a[0xc4 / 4];
    void *ms = ((S0)fn(O(1168, 1165)))();
    ((T1)fn(O(561, 561)))(a, (unsigned)T("probe"));
    printf("CWinApp app %d thread %d name %s autodelete %u tid %d handle %d\n", at(ms, 4) == (unsigned)a, ((S0)fn(O(1175, 1172)))() == a, ps((const TCH *)a[0x7c / 4]), a[0x28 / 4], a[0x30 / 4] == GetCurrentThreadId(), (HANDLE)a[0x2c / 4] == GetCurrentThread());
    void *inst = GetModuleHandleA(0);
    int ok = (int)(size_t)((S4)fn(O(1575, 1568)))((unsigned)inst, 0, (unsigned)T(""), 1);
    const TCH *prof = (const TCH *)a[0x94 / 4];
    size_t n = tlen(prof);
    printf("AfxWinInit %d instance %d resources %d app instance %d show %u appname %s exe %d profile .INI %d\n", ok, at(ms, 8) == (unsigned)inst, at(ms, 12) == (unsigned)inst, a[0x6c / 4] == (unsigned)inst, a[0x78 / 4], ps((const TCH *)at(ms, 0x10)), a[0x8c / 4] != 0, n > 4 && !strcmp(ps(prof + n - 4), ".INI"));
    windows();
    ((T0)fn(O(815, 815)))(a);
    printf("after ~CWinApp app %u\n", at(ms, 4));
}

static void files() {
    unsigned f[4];
    ((T0)fn(O(354, 354)))(f);
    printf("CFile hFile %d close %u name empty %d\n", (int)f[1], f[2], at((void *)f[3], -8) == 0);
    unsigned e[5];
    memset(e, 0, sizeof e);
    ((T0)fn(O(540, 540)))(&e[4]);
    int ok = (int)(size_t)((T3)fn(O(5186, 5180)))(f, (unsigned)T("no such file.bin"), 0, (unsigned)e);
    printf("open missing %d cause %u oserror %u name %s\n", ok, e[2], e[3], ps((TCH *)e[4]));
    DeleteFileA("mfc42probe.bin");
    ok = (int)(size_t)((T3)fn(O(5186, 5180)))(f, (unsigned)T("mfc42probe.bin"), 0x1001, 0);
    printf("create %d close %u\n", ok, f[2]);
    ((T2)fn(O(6385, 6381)))(f, (unsigned)"abcdef", 6);
    printf("length %u position %u\n", (unsigned)(size_t)((void *(__thiscall *)(void *))fn(O(3318, 3313)))(f), (unsigned)(size_t)((void *(__thiscall *)(void *))fn(O(3507, 3502)))(f));
    Str nm;
    ((T1)fn(O(3176, 3171)))(f, (unsigned)&nm);
    printf("file name %s\n", ps(nm.p));
    dtor(&nm);
    ((T0)fn(O(1979, 1971)))(f);
    printf("closed hFile %d\n", (int)f[1]);
    ok = (int)(size_t)((T3)fn(O(5186, 5180)))(f, (unsigned)T("mfc42probe.bin"), 0, 0);
    char buf[16];
    memset(buf, 0, sizeof buf);
    unsigned got = (unsigned)(size_t)((T2)fn(O(5442, 5438)))(f, (unsigned)buf, 16);
    printf("read %d got %u %s\n", ok, got, buf);
    ((T2)fn(O(5773, 5769)))(f, 2, 0);
    memset(buf, 0, sizeof buf);
    got = (unsigned)(size_t)((T2)fn(O(5442, 5438)))(f, (unsigned)buf, 3);
    printf("seek read %u %s\n", got, buf);
    ((T0)fn(O(1979, 1971)))(f);
    ((T0)fn(O(665, 665)))(f);
    DeleteFileA("mfc42probe.bin");
    // MFC throws a CFileException* (caught by its class or a base's)
    for (int k = 0; k < 3; k++) {
        try {
            if (k == 2)
                ((S3)fn(O(1262, 1255)))(13, 112, (unsigned)T("disk.bin"));
            else
                ((S2)fn(O(6264, 6260)))(k == 0 ? 2 : 5, (unsigned)T("thrown.bin"));
            printf("not thrown\n");
        } catch (CFileException *x) {
            TCH msg[128];
            unsigned help = 0;
            int r = (int)(size_t)((void *(__thiscall *)(void *, TCH *, unsigned, unsigned *))fn(O(3127, 3122)))(x, msg, 128, &help);
            printf("caught CFileException cause %u oserror %u autodelete %u name %s message %d \"%s\" help %#x\n", at(x, 8), at(x, 12), at(x, 4), ps(*(TCH **)((char *)x + 16)), r, ps(msg), help);
            ((T0)fn(O(2393, 2385)))(x);
        } catch (CException *) {
            printf("caught CException\n");
        }
    }
    try {
        ((S2)fn(O(6264, 6260)))(3, (unsigned)T("as base"));
    } catch (CException *x) {
        printf("caught as CException cause %u\n", at(x, 8));
        ((T0)fn(O(2393, 2385)))(x);
    }
    try {
        ((S2)fn(O(6264, 6260)))(0, (unsigned)T("no error"));
        printf("ThrowOsError(0) returns\n");
    } catch (CObject *) {
        printf("ThrowOsError(0) threw\n");
    }
}

int main() {
    mfc = LoadLibraryA(DLL);
    if (!mfc) {
        printf("skip no %s\n", DLL);
        return 0;
    }
    strings();
    classes();
    maps();
    app();
    files();
    return 0;
}
