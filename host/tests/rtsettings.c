/* Windows Runtime classes a desktop program activates for its look: UISettings (AdvHD x64
 * reads the theme's colors) and AccessibilitySettings (Unity 6's App UI plugin,
 * AppUINativePlugin.dll, asks whether high contrast is on; C++/WinRT throws
 * REGDB_E_CLASSNOTREG when the class is missing and onihaz's trial ended at startup). Each
 * through its factory and through RoActivateInstance, each object answering for its own
 * interfaces only. The expected lines are what Windows gives (high contrast off).
 * (sake: winrt.rs) */
#include <stdio.h>
#include <string.h>
#include <windows.h>

typedef void *HSTRG;
typedef struct Obj { void **v; } Obj;
typedef HRESULT (WINAPI *QI)(Obj *, const GUID *, Obj **);
typedef ULONG (WINAPI *REL)(Obj *);
typedef HRESULT (WINAPI *NAME)(Obj *, HSTRG *);
typedef HRESULT (WINAPI *ACT)(Obj *, Obj **);
typedef HRESULT (WINAPI *GETB)(Obj *, unsigned char *);
typedef HRESULT (WINAPI *GETS)(Obj *, HSTRG *);
typedef HRESULT (WINAPI *COLOR)(Obj *, int, unsigned char *);

static HRESULT (WINAPI *pCreate)(const WCHAR *, UINT32, HSTRG *);
static HRESULT (WINAPI *pDelete)(HSTRG);
static const WCHAR *(WINAPI *pRaw)(HSTRG, UINT32 *);
static HRESULT (WINAPI *pRoInit)(int);
static HRESULT (WINAPI *pFactory)(HSTRG, const GUID *, void **);
static HRESULT (WINAPI *pActivate)(HSTRG, void **);

static const GUID IID_Factory = {0x35, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
static const GUID IID_Access = {0xfe0e8147, 0xc4c0, 0x4562, {0xb9, 0x62, 0x13, 0x27, 0xb5, 0x2a, 0xd5, 0xb9}};
static const GUID IID_UI = {0x85361600, 0x1c63, 0x4627, {0xbc, 0xb1, 0x3a, 0x89, 0xe0, 0xbc, 0x9c, 0x55}};
static const GUID IID_UI3 = {0x03021be4, 0x5254, 0x4781, {0x81, 0x94, 0x51, 0x68, 0xf7, 0xd0, 0x6d, 0x7b}};

static HSTRG str(const WCHAR *s)
{
    HSTRG h = 0;
    pCreate(s, (UINT32)wcslen(s), &h);
    return h;
}

static void name(const char *what, Obj *o)
{
    HSTRG h = 0;
    HRESULT hr = ((NAME)o->v[4])(o, &h);
    UINT32 n = 0;
    const WCHAR *s = pRaw(h, &n);
    char a[128];
    int k = WideCharToMultiByte(CP_UTF8, 0, s, (int)n, a, sizeof(a) - 1, 0, 0);
    a[k] = 0;
    printf("%s class name: %s '%s'\n", what, SUCCEEDED(hr) ? "ok" : "failed", a);
    pDelete(h);
}

static Obj *query(const char *what, Obj *o, const GUID *iid)
{
    Obj *r = (Obj *)1;
    HRESULT hr = ((QI)o->v[0])(o, iid, &r);
    printf("%s: %s, out %s\n", what, hr == S_OK ? "ok" : hr == E_NOINTERFACE ? "no interface" : "failed", r ? "set" : "NULL");
    return hr == S_OK ? r : NULL;
}

static void show_access(const char *how, Obj *o)
{
    Obj *a = query("  IAccessibilitySettings", o, &IID_Access);
    if (!a)
        return;
    unsigned char hc = 0xcc;
    HRESULT hr = ((GETB)a->v[6])(a, &hc);
    printf("  HighContrast: %s %u\n", SUCCEEDED(hr) ? "ok" : "failed", hc);
    HSTRG s = (HSTRG)1;
    hr = ((GETS)a->v[7])(a, &s);
    printf("  HighContrastScheme: %s\n", SUCCEEDED(hr) ? "ok" : "failed");
    if (SUCCEEDED(hr))
        pDelete(s);
    query("  IUISettings of an AccessibilitySettings", a, &IID_UI);
    name(how, a);
    ((REL)a->v[2])(a);
}

static void show_ui(const char *how, Obj *o)
{
    Obj *u = query("  IUISettings3", o, &IID_UI3);
    if (!u)
        return;
    unsigned char c[4] = {1, 2, 3, 4};
    HRESULT hr = ((COLOR)u->v[6])(u, 0, c); /* GetColorValue(Background) */
    printf("  background: %s alpha %u\n", SUCCEEDED(hr) ? "ok" : "failed", c[0]);
    query("  IAccessibilitySettings of a UISettings", u, &IID_Access);
    name(how, u);
    ((REL)u->v[2])(u);
}

int main(void)
{
    HMODULE m = LoadLibraryA("combase.dll");
#define GET(p, n) *(FARPROC *)&p = GetProcAddress(m, n)
    GET(pCreate, "WindowsCreateString");
    GET(pDelete, "WindowsDeleteString");
    GET(pRaw, "WindowsGetStringRawBuffer");
    GET(pRoInit, "RoInitialize");
    GET(pFactory, "RoGetActivationFactory");
    GET(pActivate, "RoActivateInstance");
    printf("RoInitialize: %s\n", SUCCEEDED(pRoInit(1)) ? "ok" : "failed");

    const WCHAR *classes[2] = {L"Windows.UI.ViewManagement.AccessibilitySettings", L"Windows.UI.ViewManagement.UISettings"};
    for (int i = 0; i < 2; i++)
    {
        HSTRG cls = str(classes[i]);
        const char *n = i ? "UISettings" : "AccessibilitySettings";
        Obj *f = (Obj *)1, *o = (Obj *)1;
        HRESULT hr = pFactory(cls, &IID_Factory, (void **)&f);
        printf("%s factory: %s\n", n, SUCCEEDED(hr) && f ? "ok" : "failed");
        if (SUCCEEDED(hr) && f)
        {
            hr = ((ACT)f->v[6])(f, &o);
            printf(" ActivateInstance: %s\n", SUCCEEDED(hr) && o ? "ok" : "failed");
            if (SUCCEEDED(hr) && o)
            {
                (i ? show_ui : show_access)(" factory's", o);
                ((REL)o->v[2])(o);
            }
            ((REL)f->v[2])(f);
        }
        o = (Obj *)1;
        hr = pActivate(cls, (void **)&o);
        printf("%s RoActivateInstance: %s\n", n, SUCCEEDED(hr) && o ? "ok" : "failed");
        if (SUCCEEDED(hr) && o)
        {
            (i ? show_ui : show_access)(" activated", o);
            ((REL)o->v[2])(o);
        }
        pDelete(cls);
    }
    return 0;
}
