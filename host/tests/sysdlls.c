/* The small system DLLs sake now implements itself instead of taking Wine's (loaded at run time
 * by name, as games do): xaudio2_7 through DllGetClassObject, xaudio2_9's X3DAudio, crtdll,
 * olepro32, dnsapi, dhcpcsvc, tbs, ncrypt, dxva2, msdmo, windowscodecs' proxy exports and
 * WICConvertBitmapSource, and LoadLibrary of DLLs that do not exist (also by a system32 path).
 * The expected lines are what Windows gives (from Wine's conformance tests of each DLL, and
 * Microsoft's documentation); built for x86 and x86-64. No C runtime.
 * libs: -nostdlib -lole32 -luser32 -lkernel32 */
#include <windows.h>

void *memset(void *d, int c, size_t n)
{
    char *p = d;
    while (n--) *p++ = (char)c;
    return d;
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

static int milli(float f) { return (int)(f * 1000.0f + (f < 0 ? -0.5f : 0.5f)); }

static const GUID CLSID_XAudio27 = {0x5a508685, 0xa254, 0x4fba, {0x9b, 0x82, 0x9a, 0x24, 0xb0, 0x03, 0x06, 0xaf}};
static const GUID IID_IClassFactory_ = {0x00000001, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
static const GUID IID_IUnknown_ = {0, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
static const GUID IID_IXAudio27 = {0x8bcf1f58, 0x9fe7, 0x4583, {0x8a, 0xc6, 0xe2, 0xad, 0xc4, 0x65, 0xc8, 0xbb}};
static const GUID IID_IXAudio2 = {0x2b02e3cf, 0x2e0b, 0x4ec3, {0xbe, 0x45, 0x1b, 0x2a, 0x3f, 0xe7, 0x21, 0x0d}};
static const GUID GUID_Bad = {0x12345678, 0x1234, 0x1234, {1, 2, 3, 4, 5, 6, 7, 8}};

typedef struct { void **vtbl; } Obj;
#define M(o, i, T) ((T)((Obj *)(o))->vtbl[i])
typedef HRESULT(WINAPI *QI)(void *, const GUID *, void **);
typedef ULONG(WINAPI *REL)(void *);

static void xaudio27(void)
{
    HMODULE m = LoadLibraryA("xaudio2_7.dll");
    out("xaudio2_7 load %d\n", m != NULL);
    if (!m) return;
    HRESULT(WINAPI * get)(const GUID *, const GUID *, void **) = (void *)GetProcAddress(m, "DllGetClassObject");
    HRESULT(WINAPI * unload)(void) = (void *)GetProcAddress(m, "DllCanUnloadNow");
    out("procs %d %d\n", get != NULL, unload != NULL);
    void *cf = (void *)1;
    HRESULT r = get(&GUID_Bad, &IID_IClassFactory_, &cf);
    out("bad class %08x null %d\n", r, cf == NULL);
    cf = NULL;
    out("factory %08x\n", get(&CLSID_XAudio27, &IID_IClassFactory_, &cf));
    if (!cf) return;
    void *u = NULL;
    r = M(cf, 0, QI)(cf, &IID_IUnknown_, &u);
    out("factory qi unknown %08x same %d\n", r, u == cf);
    if (u) M(u, 2, REL)(u);
    void *xa = (void *)1;
    HRESULT(WINAPI * create)(void *, void *, const GUID *, void **) = M(cf, 3, void *);
    r = create(cf, (void *)cf, &IID_IXAudio27, &xa);
    out("aggregate %08x null %d\n", r, xa == NULL);
    xa = NULL;
    out("create %08x\n", create(cf, NULL, &IID_IXAudio27, &xa));
    out("lock %08x\n", M(cf, 4, HRESULT(WINAPI *)(void *, BOOL))(cf, TRUE));
    out("unlock %08x\n", M(cf, 4, HRESULT(WINAPI *)(void *, BOOL))(cf, FALSE));
    M(cf, 2, REL)(cf);
    if (!xa) return;
    void *x2 = (void *)1;
    r = M(xa, 0, QI)(xa, &IID_IXAudio2, &x2);
    out("2.7 qi 2.8 %08x null %d\n", r, x2 == NULL);
    out("release %d\n", (int)M(xa, 2, REL)(xa));
}

#pragma pack(push, 1)
typedef struct { float x, y, z; } V3;
typedef struct { V3 front, top, pos, vel; void *cone; } LISTENER;
typedef struct {
    void *cone;
    V3 front, top, pos, vel;
    float inner_radius, inner_radius_angle;
    UINT32 channels;
    float channel_radius;
    float *azimuths;
    void *volume, *lfe, *lpf_direct, *lpf_reverb, *reverb;
    float curve_scaler, doppler_scaler;
} EMITTER;
typedef struct {
    float *matrix, *delays;
    UINT32 src, dst;
    float lpf_direct, lpf_reverb, reverb, doppler, angle, distance, emitter_vel, listener_vel;
} DSP;
#pragma pack(pop)

static void x3d(void)
{
    HMODULE m = LoadLibraryA("xaudio2_9.dll");
    out("xaudio2_9 load %d\n", m != NULL);
    if (!m) return;
    HRESULT(WINAPI * create)(void **, UINT32, UINT32) = (void *)GetProcAddress(m, "XAudio2Create");
    HRESULT(__cdecl * init)(UINT32, float, BYTE *) = (void *)GetProcAddress(m, "X3DAudioInitialize");
    void(__cdecl * calc)(const BYTE *, const LISTENER *, const EMITTER *, UINT32, DSP *) = (void *)GetProcAddress(m, "X3DAudioCalculate");
    out("procs %d %d %d\n", create != NULL, init != NULL, calc != NULL);
    void *xa = NULL;
    out("XAudio2Create %08x\n", create(&xa, 0, 1));
    if (xa) {
        void *u = NULL;
        HRESULT r = M(xa, 0, QI)(xa, &IID_IXAudio2, &u);
        out("qi IXAudio2 %08x same %d\n", r, u == xa);
        if (u) M(u, 2, REL)(u);
        out("release %d\n", (int)M(xa, 2, REL)(xa));
    }
    static const GUID reverb = {0x7d9aca56, 0xcb68, 0x4807, {0xb6, 0x32, 0xb1, 0x37, 0x35, 0x2e, 0x85, 0x96}};
    static const GUID iid_xapo = {0xa410b984, 0x9839, 0x4819, {0xa0, 0xbe, 0x28, 0x56, 0xae, 0x6b, 0x3a, 0xdb}};
    static const GUID iid_params = {0x26d95c66, 0x80f2, 0x499a, {0xad, 0x54, 0x5a, 0xe7, 0xf0, 0x1c, 0x6d, 0x98}};
    HRESULT(__cdecl * fx)(const GUID *, void **, const void *, UINT32) = (void *)GetProcAddress(m, "CreateFX");
    HRESULT(WINAPI * meter)(void **) = (void *)GetProcAddress(m, "CreateAudioVolumeMeter");
    void *fxo = NULL;
    HRESULT r = fx(&reverb, &fxo, NULL, 0);
    out("CreateFX reverb %08x\n", r);
    if (fxo) {
        void *x = NULL, *p = NULL;
        HRESULT r1 = M(fxo, 0, QI)(fxo, &iid_xapo, &x);
        HRESULT r2 = M(fxo, 0, QI)(fxo, &iid_params, &p);
        out("qi xapo %08x params %08x\n", r1, r2);
        if (p) {
            float set[3] = {1.5f, 2.5f, 3.5f}, get[3] = {0};
            M(p, 3, void(WINAPI *)(void *, const void *, UINT32))(p, set, sizeof set);
            M(p, 4, void(WINAPI *)(void *, void *, UINT32))(p, get, sizeof get);
            out("params %d %d %d\n", milli(get[0]), milli(get[1]), milli(get[2]));
            M(p, 2, REL)(p);
        }
        if (x) M(x, 2, REL)(x);
        out("fx release %d\n", (int)M(fxo, 2, REL)(fxo));
    }
    fxo = NULL;
    out("meter %08x\n", meter(&fxo));
    if (fxo) {
        /* a 2-channel float lock, one Process of 2 frames, the levels */
        WAVEFORMATEX wf = {3, 2, 48000, 48000 * 8, 8, 32, 0};
        struct { const WAVEFORMATEX *f; UINT32 max; } lk = {&wf, 64};
        HRESULT lr = M(fxo, 8, HRESULT(WINAPI *)(void *, UINT32, void *, UINT32, void *))(fxo, 1, &lk, 1, &lk);
        float buf[4] = {0.5f, -1.0f, -0.5f, 0.0f};
        struct { void *b; UINT32 flags, frames; } pb = {buf, 1, 2};
        M(fxo, 10, void(WINAPI *)(void *, UINT32, void *, UINT32, void *, BOOL))(fxo, 1, &pb, 1, &pb, TRUE);
        void *p = NULL;
        M(fxo, 0, QI)(fxo, &iid_params, &p);
        float peak[2] = {0}, rms[2] = {0};
        struct { float *peak, *rms; UINT32 n; } lv = {peak, rms, 2};
        if (p) {
            M(p, 4, void(WINAPI *)(void *, void *, UINT32))(p, &lv, sizeof lv);
            M(p, 2, REL)(p);
        }
        out("meter lock %08x peak %d %d rms %d %d\n", lr, milli(peak[0]), milli(peak[1]), milli(rms[0]), milli(rms[1]));
        out("meter release %d\n", (int)M(fxo, 2, REL)(fxo));
    }
    BYTE h[20];
    out("init bad speed %08x\n", init(3, 0.0f, h));
    out("init stereo %08x\n", init(3, 343.5f, h));
    LISTENER l = {{0, 0, 1}, {0, 1, 0}, {0, 0, 0}, {0, 0, 0}, NULL};
    EMITTER e;
    memset(&e, 0, sizeof e);
    e.front.z = 1;
    e.top.y = 1;
    e.channels = 1;
    e.curve_scaler = 1;
    e.doppler_scaler = 1;
    float mat[2];
    DSP d;
    memset(&d, 0, sizeof d);
    d.matrix = mat;
    d.src = 1;
    d.dst = 2;
    static const V3 at[] = {{1, 0, 0}, {-2, 0, 0}, {0, 0, 4}, {0, 0, 0.5f}};
    for (int i = 0; i < 4; i++) {
        e.pos = at[i];
        calc(h, &l, &e, 0x1 | 0x40, &d);
        out("emitter %d: L %d R %d distance %d angle %d\n", i, milli(mat[0]), milli(mat[1]), milli(d.distance), milli(d.angle));
    }
    e.pos = (V3){0, 0, 10};
    e.vel = (V3){0, 0, -34.35f};
    calc(h, &l, &e, 0x1 | 0x20, &d);
    out("approaching doppler %d\n", milli(d.doppler));
}

static void crtdll(void)
{
    HMODULE m = LoadLibraryA("crtdll.dll");
    out("crtdll load %d\n", m != NULL);
    if (!m) return;
    unsigned short **ct = (void *)GetProcAddress(m, "_pctype_dll");
    unsigned short *t = (void *)GetProcAddress(m, "_ctype");
    int(__cdecl * len)(const char *) = (void *)GetProcAddress(m, "strlen");
    int(__cdecl * sp)(char *, const char *, ...) = (void *)GetProcAddress(m, "sprintf");
    out("procs %d %d %d %d\n", ct != NULL, t != NULL, len != NULL, sp != NULL);
    if (t) out("ctype A %x a %x 0 %x space %x\n", t[1 + 'A'], t[1 + 'a'], t[1 + '0'], t[1 + ' ']);
    if (ct && *ct) out("pctype A %x\n", (*ct)['A']);
    char b[32];
    if (sp && len) {
        sp(b, "%d-%s-%x", 42, "ab", 255);
        out("sprintf %s %d\n", b, len(b));
    }
}

static void olepro32(void)
{
    HMODULE m = LoadLibraryA("olepro32.dll");
    out("olepro32 load %d\n", m != NULL);
    if (!m) return;
    out("procs %d %d %d\n", GetProcAddress(m, "OleCreateFontIndirect") != NULL, GetProcAddress(m, "OleCreatePictureIndirect") != NULL,
        GetProcAddress(m, "DllGetClassObject") != NULL);
}

typedef struct { void *next; WCHAR *name; WORD type, len; DWORD flags, ttl, reserved; BYTE data[16]; } DNSREC;

static void dnsapi(void)
{
    HMODULE m = LoadLibraryA("dnsapi.dll");
    out("dnsapi load %d\n", m != NULL);
    if (!m) return;
    LONG(WINAPI * val)(const WCHAR *, int) = (void *)GetProcAddress(m, "DnsValidateName_W");
    BOOL(WINAPI * cmp)(const char *, const char *) = (void *)GetProcAddress(m, "DnsNameCompare_A");
    LONG(WINAPI * q)(const char *, WORD, DWORD, void *, DNSREC **, void *) = (void *)GetProcAddress(m, "DnsQuery_A");
    void(WINAPI * fr)(void *, int) = (void *)GetProcAddress(m, "DnsRecordListFree");
    out("procs %d %d %d %d\n", val != NULL, cmp != NULL, q != NULL, fr != NULL);
    static const WCHAR *names[] = {L"", L"a", L"a.b", L"1.2.3", L"a_b", L"a b", L"a*", L"*.a", L"a..b", L".", L"a."};
    for (int i = 0; i < 11; i++) out("validate %d: %d %d\n", i, (int)val(names[i], 0), (int)val(names[i], 1));
    out("compare %d %d %d\n", cmp("example.com", "EXAMPLE.COM."), cmp("a.b", "a.c"), cmp("a.", "a"));
    DNSREC *r = NULL;
    LONG s = q("127.0.0.1", 1, 0, NULL, &r, NULL);
    out("query literal %d", (int)s);
    if (!s && r) out(" type %d len %d ip %d.%d.%d.%d", r->type, r->len, r->data[0], r->data[1], r->data[2], r->data[3]);
    out("\n");
    if (r) fr(r, 1);
    r = NULL;
    out("query bad name %d\n", (int)q(" a", 1, 0, NULL, &r, NULL));
    r = NULL;
    s = q("localhost", 1, 0, NULL, &r, NULL);
    out("query localhost %d", (int)s);
    if (!s && r) out(" ip %d.%d.%d.%d", r->data[0], r->data[1], r->data[2], r->data[3]);
    out("\n");
    if (r) fr(r, 1);
    /* a wire-format answer for winehq.org (header counts in host order) */
    static BYTE msg[] = {0x12, 0x34, 0x81, 0x80, 1, 0, 1, 0, 0, 0, 0, 0, 6, 'w', 'i', 'n', 'e', 'h', 'q', 3, 'o', 'r', 'g', 0, 0, 1, 0, 1,
                         0xc0, 0x0c, 0, 1, 0, 1, 4, 5, 6, 7, 0, 4, 4, 4, 0x51, 0x7c};
    LONG(WINAPI * ex)(void *, WORD, DNSREC **) = (void *)GetProcAddress(m, "DnsExtractRecordsFromMessage_W");
    BOOL(WINAPI * rc)(DNSREC *, DNSREC *) = (void *)GetProcAddress(m, "DnsRecordCompare");
    DNSREC *a = NULL, *b = NULL;
    s = ex(msg, sizeof msg, &a);
    out("extract %d", (int)s);
    if (!s && a) out(" type %d ttl %x ip %d.%d.%d.%d section %d charset %d", a->type, (int)a->ttl, a->data[0], a->data[1], a->data[2], a->data[3], (int)(a->flags & 3), (int)(a->flags >> 3 & 3));
    out("\n");
    out("extract short %d\n", (int)ex(msg, 12, &b));
    b = NULL;
    ex(msg, sizeof msg, &b);
    if (a && b) {
        out("compare same %d", rc(a, b));
        b->data[3] = 0;
        out(" changed %d\n", rc(a, b));
    }
    if (a) fr(a, 1);
    if (b) fr(b, 1);
}

static void small(void)
{
    HMODULE m = LoadLibraryA("dhcpcsvc.dll");
    out("dhcpcsvc load %d\n", m != NULL);
    if (m) {
        DWORD(WINAPI * init)(DWORD *) = (void *)GetProcAddress(m, "DhcpCApiInitialize");
        void(WINAPI * clean)(void) = (void *)GetProcAddress(m, "DhcpCApiCleanup");
        DWORD v = 0;
        DWORD r = init(&v);
        out("initialize %d version %d\n", (int)r, (int)v);
        clean();
    }
    m = LoadLibraryA("ncrypt.dll");
    out("ncrypt load %d\n", m != NULL);
    if (m) {
        LONG(WINAPI * open)(ULONG_PTR *, const WCHAR *, DWORD) = (void *)GetProcAddress(m, "NCryptOpenStorageProvider");
        LONG(WINAPI * fo)(ULONG_PTR) = (void *)GetProcAddress(m, "NCryptFreeObject");
        ULONG_PTR p = 0;
        LONG r = open(&p, L"Microsoft Software Key Storage Provider", 0);
        out("open %08x %d\n", (int)r, p != 0);
        r = fo(p);
        out("free %08x free null %08x\n", (int)r, (int)fo(0));
    }
    m = LoadLibraryA("dxva2.dll");
    out("dxva2 load %d\n", m != NULL);
    if (m) {
        HRESULT(WINAPI * mk)(UINT *, void **) = (void *)GetProcAddress(m, "DXVA2CreateDirect3DDeviceManager9");
        UINT token = 0;
        void *dm = NULL;
        HRESULT r = mk(&token, &dm);
        out("manager %08x %d\n", (int)r, dm != NULL);
        if (dm) {
            HANDLE h = (HANDLE)1;
            out("open no device %08x\n", (int)M(dm, 4, HRESULT(WINAPI *)(void *, HANDLE *))(dm, &h));
            out("reset null %08x\n", (int)M(dm, 3, HRESULT(WINAPI *)(void *, void *, UINT))(dm, NULL, token));
            out("release %d\n", (int)M(dm, 2, REL)(dm));
        }
    }
    m = LoadLibraryA("msdmo.dll");
    out("msdmo load %d\n", m != NULL);
    if (m) {
        HRESULT(WINAPI * ini)(void *, DWORD) = (void *)GetProcAddress(m, "MoInitMediaType");
        HRESULT(WINAPI * fre)(void *) = (void *)GetProcAddress(m, "MoFreeMediaType");
        HRESULT(WINAPI * cre)(void **, DWORD) = (void *)GetProcAddress(m, "MoCreateMediaType");
        HRESULT(WINAPI * del)(void *) = (void *)GetProcAddress(m, "MoDeleteMediaType");
        BYTE mt[96];
        memset(mt, 0xcc, sizeof mt);
        out("init %08x\n", (int)ini(mt, 16));
        out("free %08x\n", (int)fre(mt));
        void *p = NULL;
        HRESULT r = cre(&p, 8);
        out("create %08x %d\n", (int)r, p != NULL);
        if (p) out("delete %08x\n", (int)del(p));
    }
}

static const GUID BGRA = {0x6fddc324, 0x4e03, 0x4bfe, {0xb1, 0x85, 0x3d, 0x77, 0x76, 0x8d, 0xc9, 0x0f}};
static const GUID PBGRA = {0x6fddc324, 0x4e03, 0x4bfe, {0xb1, 0x85, 0x3d, 0x77, 0x76, 0x8d, 0xc9, 0x10}};

static void wic(void)
{
    HMODULE m = LoadLibraryA("windowscodecs.dll");
    out("windowscodecs load %d\n", m != NULL);
    if (!m) return;
    HRESULT(WINAPI * mkf)(UINT, void **) = (void *)GetProcAddress(m, "WICCreateImagingFactory_Proxy");
    HRESULT(WINAPI * frommem)(void *, UINT, UINT, const GUID *, UINT, UINT, BYTE *, void **) =
        (void *)GetProcAddress(m, "IWICImagingFactory_CreateBitmapFromMemory_Proxy");
    HRESULT(WINAPI * size)(void *, UINT *, UINT *) = (void *)GetProcAddress(m, "IWICBitmapSource_GetSize_Proxy");
    HRESULT(WINAPI * fmt)(void *, GUID *) = (void *)GetProcAddress(m, "IWICBitmapSource_GetPixelFormat_Proxy");
    HRESULT(WINAPI * copy)(void *, const void *, UINT, UINT, BYTE *) = (void *)GetProcAddress(m, "IWICBitmapSource_CopyPixels_Proxy");
    HRESULT(WINAPI * conv)(const GUID *, void *, void **) = (void *)GetProcAddress(m, "WICConvertBitmapSource");
    out("procs %d %d %d %d %d %d\n", mkf != NULL, frommem != NULL, size != NULL, fmt != NULL, copy != NULL, conv != NULL);
    void *f = NULL;
    out("factory %08x\n", (int)mkf(0x237, &f));
    if (!f) return;
    BYTE px[8] = {200, 100, 50, 128, 10, 20, 30, 255};
    void *b = NULL;
    out("from memory %08x\n", (int)frommem(f, 2, 1, &BGRA, 8, 8, px, &b));
    if (!b) return;
    UINT w = 0, hgt = 0;
    HRESULT r = size(b, &w, &hgt);
    out("size %08x %dx%d\n", (int)r, w, hgt);
    void *pb = NULL;
    out("convert %08x\n", (int)conv(&PBGRA, b, &pb));
    if (pb) {
        GUID g;
        fmt(pb, &g);
        out("format pbgra %d\n", g.Data1 == PBGRA.Data1 && g.Data4[7] == PBGRA.Data4[7]);
        BYTE o[8] = {0};
        r = copy(pb, NULL, 8, 8, o);
        out("copy %08x %d %d %d %d / %d %d %d %d\n", (int)r, o[0], o[1], o[2], o[3], o[4], o[5], o[6], o[7]);
        M(pb, 2, REL)(pb);
    }
    void *same = NULL;
    out("convert same %08x\n", (int)conv(&BGRA, b, &same));
    if (same) M(same, 2, REL)(same);
    M(b, 2, REL)(b);
    M(f, 2, REL)(f);
}

static void missing(void)
{
    SetLastError(0);
    HMODULE m = LoadLibraryA("nosuchdll_sake.dll");
    DWORD e = GetLastError();
    out("missing %d error %d\n", m != NULL, (int)e);
    SetLastError(0);
    m = LoadLibraryA("C:\\windows\\system32\\nosuchdll_sake.dll");
    e = GetLastError();
    out("missing system32 %d error %d\n", m != NULL, (int)e);
    m = LoadLibraryA("C:\\windows\\system32\\kernel32.dll");
    HMODULE k = GetModuleHandleA("kernel32.dll");
    out("kernel32 by path %d same %d\n", m != NULL, m == k);
}

void mainCRTStartup(void)
{
    CoInitialize(NULL);
    xaudio27();
    x3d();
    crtdll();
    olepro32();
    dnsapi();
    small();
    wic();
    missing();
    ExitProcess(0);
}
