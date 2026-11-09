/* libs: -luser32 -lshell32 -luxtheme -ldwmapi -lole32 -luuid */
/* What Inno Setup's uninstaller calls around its windows: buffered paint, themes, DWM
 * composition, ghosting, shutdown block reasons, the message filter, SHChangeNotify and
 * SHCreateItemFromParsingName. sake answers as an unthemed desktop without composition (the
 * classic look it draws), so this is not compared with Windows by tests/winref: there
 * IsAppThemed and DwmIsCompositionEnabled say TRUE. (sake: misc.rs init) */
#define COBJMACROS
#include <windows.h>
#include <uxtheme.h>
#include <dwmapi.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <stdio.h>

int main(void) {
    printf("BufferedPaintInit: %lx\n", BufferedPaintInit());
    printf("IsAppThemed: %d\n", IsAppThemed());
    BOOL on = 7;
    HRESULT hr = DwmIsCompositionEnabled(&on);
    printf("DwmIsCompositionEnabled: %lx, %d\n", hr, on);
    DisableProcessWindowsGhosting();
    HWND h = CreateWindowExA(0, "STATIC", "", WS_POPUP, 0, 0, 10, 10, NULL, NULL, NULL, NULL);
    printf("ShutdownBlockReasonCreate: %d\n", ShutdownBlockReasonCreate(h, L"busy"));
    printf("ShutdownBlockReasonDestroy: %d\n", ShutdownBlockReasonDestroy(h));
    CHANGEFILTERSTRUCT cf = {sizeof cf, 99};
    BOOL r = ChangeWindowMessageFilterEx(h, WM_COPYDATA, MSGFLT_ALLOW, &cf);
    printf("ChangeWindowMessageFilterEx: %d, ext status %lu\n", r, cf.ExtStatus);
    printf("without the struct: %d\n", ChangeWindowMessageFilterEx(h, WM_COPYDATA, MSGFLT_ALLOW, NULL));
    SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, NULL, NULL);
    printf("SHChangeNotify returned\n");
    void *item = (void *)1;
    hr = SHCreateItemFromParsingName(L"C:\\", NULL, &IID_IShellItem, &item);
    printf("SHCreateItemFromParsingName: %s, item %s\n", hr == S_OK ? "S_OK" : "failed", item ? "set" : "NULL");
    if (hr == S_OK)
        IUnknown_Release((IUnknown *)item);
    DestroyWindow(h);
    printf("BufferedPaintUnInit: %lx\n", BufferedPaintUnInit());
    return 0;
}
