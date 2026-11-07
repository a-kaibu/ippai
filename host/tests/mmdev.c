// mmdevapi's device enumerator as games use it before XAudio2 / WASAPI (x64): the default
// render endpoint, its id, state and names, the endpoint collection, no capture endpoint,
// GetDevice by id, notifications, bad arguments. Prints what sake's one browser endpoint
// gives (a machine's own devices differ, so this is not a winref probe).
#define COBJMACROS
#define INITGUID
#include <windows.h>
#include <initguid.h>
#include <mmdeviceapi.h>
#include <functiondiscoverykeys_devpkey.h>
#include <stdio.h>

static HRESULT (WINAPI *pCoInitializeEx)(void *, DWORD);
static HRESULT (WINAPI *pCoCreateInstance)(REFCLSID, IUnknown *, DWORD, REFIID, void **);
static void (WINAPI *pCoTaskMemFree)(void *);
static HRESULT (WINAPI *pPropVariantClear)(PROPVARIANT *);

static void wout(const char *what, const WCHAR *s) {
    printf("%s [", what);
    for (; s && *s; s++) printf(*s < 128 ? "%c" : "<%04x>", *s);
    printf("]\n");
}

int main(void) {
    HMODULE ole = LoadLibraryA("ole32.dll");
    pCoInitializeEx = (void *)GetProcAddress(ole, "CoInitializeEx");
    pCoCreateInstance = (void *)GetProcAddress(ole, "CoCreateInstance");
    pCoTaskMemFree = (void *)GetProcAddress(ole, "CoTaskMemFree");
    pPropVariantClear = (void *)GetProcAddress(ole, "PropVariantClear");
    pCoInitializeEx(NULL, COINIT_MULTITHREADED);
    IMMDeviceEnumerator *en = NULL;
    HRESULT hr = pCoCreateInstance(&CLSID_MMDeviceEnumerator, NULL, CLSCTX_ALL, &IID_IMMDeviceEnumerator, (void **)&en);
    printf("create %08lx %d\n", hr, en != NULL);
    if (!en) return 1;
    IUnknown *unk = NULL;
    printf("qi unknown %08lx same %d\n", IMMDeviceEnumerator_QueryInterface(en, &IID_IUnknown, (void **)&unk), unk == (IUnknown *)en);
    void *other = (void *)1;
    printf("qi device %08lx %p\n", IMMDeviceEnumerator_QueryInterface(en, &IID_IMMDevice, &other), other);

    IMMDevice *dev = NULL;
    printf("default render %08lx %d\n", IMMDeviceEnumerator_GetDefaultAudioEndpoint(en, eRender, eConsole, &dev), dev != NULL);
    IMMDevice *cap = (IMMDevice *)1;
    printf("default capture %08lx %p\n", IMMDeviceEnumerator_GetDefaultAudioEndpoint(en, eCapture, eConsole, &cap), (void *)cap);
    printf("bad flow %08lx null out %08lx\n", IMMDeviceEnumerator_GetDefaultAudioEndpoint(en, 7, eConsole, &cap),
        IMMDeviceEnumerator_GetDefaultAudioEndpoint(en, eRender, eConsole, NULL));
    LPWSTR id = NULL;
    printf("id %08lx\n", IMMDevice_GetId(dev, &id));
    wout("id", id);
    DWORD state = 0;
    printf("state %08lx %lu\n", IMMDevice_GetState(dev, &state), state);
    IMMDevice *again = NULL;
    printf("get by id %08lx same %d\n", IMMDeviceEnumerator_GetDevice(en, id, &again), again == dev);
    printf("get bad id %08lx\n", IMMDeviceEnumerator_GetDevice(en, L"{nope}", &again));
    pCoTaskMemFree(id);

    // the device's IMMEndpoint (Windows' device object has it; Unity 6 asks for it): its data
    // flow, and COM identity through it
    IMMEndpoint *ep = NULL;
    printf("qi endpoint %08lx %d\n", IMMDevice_QueryInterface(dev, &IID_IMMEndpoint, (void **)&ep), ep != NULL);
    EDataFlow flow = 7;
    printf("flow %08lx %d null %08lx\n", IMMEndpoint_GetDataFlow(ep, &flow), flow, IMMEndpoint_GetDataFlow(ep, NULL));
    IUnknown *u1 = NULL, *u2 = NULL;
    IMMDevice *back = NULL;
    IMMEndpoint_QueryInterface(ep, &IID_IUnknown, (void **)&u1);
    IMMDevice_QueryInterface(dev, &IID_IUnknown, (void **)&u2);
    printf("identity %d device back %08lx same %d\n", u1 == u2 && u1 != NULL, IMMEndpoint_QueryInterface(ep, &IID_IMMDevice, (void **)&back), back == dev);
    void *none = (void *)1;
    printf("endpoint qi store %08lx %p\n", IMMEndpoint_QueryInterface(ep, &IID_IPropertyStore, &none), none);

    IPropertyStore *ps = NULL;
    printf("store %08lx %d\n", IMMDevice_OpenPropertyStore(dev, STGM_READ, &ps), ps != NULL);
    DWORD n = 0;
    IPropertyStore_GetCount(ps, &n);
    printf("props %lu\n", n);
    PROPVARIANT v;
    PropVariantInit(&v);
    printf("friendly %08lx vt %d\n", IPropertyStore_GetValue(ps, &PKEY_Device_FriendlyName, &v), v.vt);
    wout("friendly", v.vt == VT_LPWSTR ? v.pwszVal : NULL);
    pPropVariantClear(&v);
    printf("desc %08lx vt %d\n", IPropertyStore_GetValue(ps, &PKEY_Device_DeviceDesc, &v), v.vt);
    pPropVariantClear(&v);
    printf("other key %08lx vt %d\n", IPropertyStore_GetValue(ps, &PKEY_AudioEndpoint_FormFactor, &v), v.vt);
    PROPERTYKEY k;
    printf("key 0 %08lx pid %lu, key %lu %08lx\n", IPropertyStore_GetAt(ps, 0, &k), k.pid, n, IPropertyStore_GetAt(ps, n, &k));
    printf("set %08lx\n", IPropertyStore_SetValue(ps, &PKEY_Device_FriendlyName, &v));

    IMMDeviceCollection *col = NULL;
    UINT cn = 99;
    printf("enum render %08lx ", IMMDeviceEnumerator_EnumAudioEndpoints(en, eRender, DEVICE_STATE_ACTIVE, &col));
    IMMDeviceCollection_GetCount(col, &cn);
    IMMDevice *item = NULL;
    printf("count %u item0 %08lx same %d item1 %08lx\n", cn, IMMDeviceCollection_Item(col, 0, &item), item == dev,
        IMMDeviceCollection_Item(col, 1, &item));
    printf("enum capture %08lx ", IMMDeviceEnumerator_EnumAudioEndpoints(en, eCapture, DEVICE_STATE_ACTIVE, &col));
    IMMDeviceCollection_GetCount(col, &cn);
    printf("count %u\n", cn);
    printf("enum unplugged %08lx ", IMMDeviceEnumerator_EnumAudioEndpoints(en, eAll, DEVICE_STATE_UNPLUGGED, &col));
    IMMDeviceCollection_GetCount(col, &cn);
    printf("count %u bad mask %08lx\n", cn, IMMDeviceEnumerator_EnumAudioEndpoints(en, eAll, 0x10, &col));
    printf("notify null %08lx\n", IMMDeviceEnumerator_RegisterEndpointNotificationCallback(en, NULL));
    fflush(stdout);
    return 0;
}
