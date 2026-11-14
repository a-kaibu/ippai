/* Keys every Windows has, even on a fresh install, opened the ways setup programs open them to
 * tell whether they may install (EXEpress CX opens HKLM\...\Uninstall and stops with "requires
 * power user" when it can't), and a key that does not exist. Read access only: the answers
 * must not depend on the runner being an administrator. */
#include "../probe.h"

static void open1(HKEY root, const char *rn, const char *sub) {
    HKEY k = NULL;
    LONG r = RegOpenKeyExA(root, sub, 0, KEY_READ, &k);
    printf("open %s\\%s = %ld\n", rn, sub, r);
    if (r == 0) {
        DWORD n = 0xdead, maxlen;
        RegQueryInfoKeyA(k, NULL, NULL, NULL, &n, &maxlen, NULL, NULL, NULL, NULL, NULL, NULL);
        RegCloseKey(k);
    }
    r = RegOpenKeyA(root, sub, &k);
    printf("open (RegOpenKey) %s\\%s = %ld\n", rn, sub, r);
    if (r == 0) RegCloseKey(k);
}

int main(void) {
    static const char *lm[] = {
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion",
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths",
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run",
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\RunOnce",
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\",
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\no such key for masu",
    };
    static const char *cu[] = {
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        "Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce",
        "Control Panel\\Desktop",
        "Control Panel\\no such key for masu",
    };
    for (int i = 0; i < (int)(sizeof lm / sizeof *lm); i++) open1(HKEY_LOCAL_MACHINE, "HKLM", lm[i]);
    for (int i = 0; i < (int)(sizeof cu / sizeof *cu); i++) open1(HKEY_CURRENT_USER, "HKCU", cu[i]);
    /* the Uninstall key is found among its parent's subkeys, under its own spelling */
    HKEY k;
    if (RegOpenKeyExA(HKEY_LOCAL_MACHINE, "SOFTWARE\\Microsoft\\Windows\\CurrentVersion", 0, KEY_READ, &k) == 0) {
        char name[256];
        int found = 0;
        for (DWORD i = 0;; i++) {
            DWORD len = sizeof name;
            if (RegEnumKeyExA(k, i, name, &len, NULL, NULL, NULL, NULL) != 0) break;
            if (!strcmp(name, "Uninstall")) found = 1;
        }
        printf("enum CurrentVersion has Uninstall %d\n", found);
        RegCloseKey(k);
    }
    return 0;
}
