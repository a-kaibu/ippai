/* FindFirstChangeNotification / FindNextChangeNotification / FindCloseChangeNotification
 * (オレンジポケット watches its save folder; sake answered 0): a folder gives a handle that
 * nothing signals while nothing changes; a missing folder ERROR_FILE_NOT_FOUND, a file
 * ERROR_DIRECTORY, both INVALID_HANDLE_VALUE; FindNext re-arms, FindClose closes. */
#include <windows.h>
#include <stdio.h>

int main(void)
{
    char dir[MAX_PATH], self[MAX_PATH], missing[MAX_PATH];
    GetTempPathA(MAX_PATH, dir);
    GetModuleFileNameA(NULL, self, MAX_PATH);
    snprintf(missing, MAX_PATH, "%snosuch-changenotify-dir", dir);
    DWORD filters[] = { FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_DIR_NAME };
    for (int i = 0; i < 2; i++) {
        HANDLE h = i ? FindFirstChangeNotificationW(L".", TRUE, filters[i]) : FindFirstChangeNotificationA(dir, FALSE, filters[i]);
        printf("folder %c: valid %d\n", i ? 'W' : 'A', h != INVALID_HANDLE_VALUE && h != NULL);
        printf("  wait %lu\n", WaitForSingleObject(h, 0));
        printf("  next %d wait %lu\n", FindNextChangeNotification(h), WaitForSingleObject(h, 10));
        printf("  close %d\n", FindCloseChangeNotification(h));
    }
    SetLastError(0);
    HANDLE m = FindFirstChangeNotificationA(missing, FALSE, FILE_NOTIFY_CHANGE_FILE_NAME);
    printf("missing folder: invalid %d err %lu\n", m == INVALID_HANDLE_VALUE, GetLastError());
    SetLastError(0);
    HANDLE f = FindFirstChangeNotificationA(self, FALSE, FILE_NOTIFY_CHANGE_FILE_NAME);
    printf("a file: invalid %d err %lu\n", f == INVALID_HANDLE_VALUE, GetLastError());
    return 0;
}
