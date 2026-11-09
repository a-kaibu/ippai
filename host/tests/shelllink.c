/* libs: -lole32 -luuid -lshell32 */
/* Shortcuts as installers make them (Inno Setup's [Icons], NSIS CreateShortCut): a ShellLink
 * object gets a target, arguments, working directory, description, icon, show command and hot
 * key through IShellLinkW, is saved with IPersistFile, and a second object loads the .lnk and
 * reads them back through IShellLinkW and IShellLinkA; IsDirty before and after, GetCurFile,
 * GetClassID, the interfaces it answers, and the file's header. (sake: shelllink.rs; compared
 * with Windows by tests/winref)
 */
#define COBJMACROS
#include <windows.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <stdio.h>
#include <string.h>

static char dir[MAX_PATH], self[MAX_PATH];

/* this program as <self>, its directory as <dir> */
static void shown(const char *p) {
    size_t n = strlen(dir);
    if (!_stricmp(p, self))
        printf("<self>");
    else if (!_strnicmp(p, dir, n))
        printf("<dir>%s", p + n);
    else
        printf("%s", p);
}

int main(void) {
    CoInitialize(NULL);
    char tmp[MAX_PATH], lnk[MAX_PATH];
    GetModuleFileNameA(NULL, self, sizeof self);
    strcpy(dir, self);
    *strrchr(dir, '\\') = 0;
    GetTempPathA(sizeof tmp, tmp);
    sprintf(lnk, "%sshelllink%lu.lnk", tmp, GetCurrentProcessId());
    WCHAR wself[MAX_PATH], wdir[MAX_PATH], wlnk[MAX_PATH];
    MultiByteToWideChar(CP_ACP, 0, self, -1, wself, MAX_PATH);
    MultiByteToWideChar(CP_ACP, 0, dir, -1, wdir, MAX_PATH);
    MultiByteToWideChar(CP_ACP, 0, lnk, -1, wlnk, MAX_PATH);

    IShellLinkW *sl;
    HRESULT hr = CoCreateInstance(&CLSID_ShellLink, NULL, CLSCTX_INPROC_SERVER, &IID_IShellLinkW, (void **)&sl);
    printf("create: %lx\n", hr);
    if (hr)
        return 1;
    IPersistFile *pf;
    printf("IPersistFile: %lx\n", IShellLinkW_QueryInterface(sl, &IID_IPersistFile, (void **)&pf));
    IUnknown *unk = NULL;
    printf("IStream: %lx\n", IShellLinkW_QueryInterface(sl, &IID_IStream, (void **)&unk));
    CLSID cls;
    IPersistFile_GetClassID(pf, &cls);
    printf("class is CLSID_ShellLink: %d\n", IsEqualCLSID(&cls, &CLSID_ShellLink));
    printf("dirty when new: %lx\n", IPersistFile_IsDirty(pf));
    WCHAR w[MAX_PATH];
    w[0] = 'x';
    hr = IShellLinkW_GetPath(sl, w, MAX_PATH, NULL, 0);
    printf("path when new: %lx, empty %d\n", hr, w[0] == 0);
    int show = 0;
    IShellLinkW_GetShowCmd(sl, &show);
    printf("show when new: %d\n", show);

    IShellLinkW_SetPath(sl, wself);
    IShellLinkW_SetArguments(sl, L"/run \"two words\"");
    IShellLinkW_SetWorkingDirectory(sl, wdir);
    IShellLinkW_SetDescription(sl, L"My Program");
    IShellLinkW_SetIconLocation(sl, wself, 2);
    IShellLinkW_SetShowCmd(sl, SW_SHOWMINNOACTIVE);
    IShellLinkW_SetHotkey(sl, MAKEWORD('P', HOTKEYF_CONTROL | HOTKEYF_ALT));
    printf("dirty after setting: %lx\n", IPersistFile_IsDirty(pf));
    LPOLESTR cur = (LPOLESTR)1;
    hr = IPersistFile_GetCurFile(pf, &cur);
    printf("current file before saving: %lx, %s\n", hr, cur ? "a name" : "NULL");
    printf("save: %lx\n", IPersistFile_Save(pf, wlnk, TRUE));
    printf("dirty after saving: %lx\n", IPersistFile_IsDirty(pf));
    hr = IPersistFile_GetCurFile(pf, &cur);
    printf("current file: %lx, the link %d\n", hr, hr == S_OK && !lstrcmpiW(cur, wlnk));
    if (hr == S_OK)
        CoTaskMemFree(cur);
    IPersistFile_Release(pf);
    printf("release: %lu\n", IShellLinkW_Release(sl));

    FILE *f = fopen(lnk, "rb");
    unsigned char h[24] = {0};
    if (f) {
        fread(h, 1, sizeof h, f);
        fclose(f);
    }
    printf("file header: size %02x, CLSID_ShellLink %d\n", h[0], !memcmp(h + 4, &CLSID_ShellLink, 16));

    CoCreateInstance(&CLSID_ShellLink, NULL, CLSCTX_INPROC_SERVER, &IID_IPersistFile, (void **)&pf);
    printf("load: %lx\n", IPersistFile_Load(pf, wlnk, STGM_READ));
    printf("dirty after loading: %lx\n", IPersistFile_IsDirty(pf));
    IShellLinkA *sa;
    printf("IShellLinkA: %lx\n", IPersistFile_QueryInterface(pf, &IID_IShellLinkA, (void **)&sa));
    char a[MAX_PATH];
    WIN32_FIND_DATAA fd;
    memset(&fd, 0xcc, sizeof fd);
    hr = IShellLinkA_GetPath(sa, a, MAX_PATH, &fd, SLGP_RAWPATH);
    printf("path: %lx ", hr);
    shown(a);
    printf(", find data name %s\n", !_stricmp(fd.cFileName, strrchr(self, '\\') + 1) ? "<self>" : fd.cFileName);
    IShellLinkA_GetArguments(sa, a, MAX_PATH);
    printf("arguments: %s\n", a);
    IShellLinkA_GetWorkingDirectory(sa, a, MAX_PATH);
    printf("directory: ");
    shown(a);
    printf("\n");
    IShellLinkA_GetDescription(sa, a, MAX_PATH);
    printf("description: %s\n", a);
    int icon = -1;
    IShellLinkA_GetIconLocation(sa, a, MAX_PATH, &icon);
    printf("icon: ");
    shown(a);
    printf(", %d\n", icon);
    IShellLinkA_GetShowCmd(sa, &show);
    WORD key = 0;
    IShellLinkA_GetHotkey(sa, &key);
    printf("show %d, hot key %04x\n", show, key);
    char small[6];
    IShellLinkA_GetDescription(sa, small, sizeof small);
    printf("description into 6 chars: %s\n", small);
    IShellLinkA_Release(sa);
    IPersistFile_Release(pf);
    CoCreateInstance(&CLSID_ShellLink, NULL, CLSCTX_INPROC_SERVER, &IID_IPersistFile, (void **)&pf);
    hr = IPersistFile_Load(pf, L"C:\\not-there-at-all.lnk", STGM_READ);
    printf("load what is not there: %lx\n", hr);
    IPersistFile_Release(pf);
    DeleteFileA(lnk);
    CoUninitialize();
    return 0;
}
