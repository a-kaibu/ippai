/* Two threads SendMessage to a window of a busy thread, then each registers a descriptor in a
 * table without a lock, as libtomcrypt's register_hash does (KiriKiri's sigcheck.dll, #82):
 * look it up, else take the first free slot. Its caller takes any slot but 0 for an error.
 * On Windows the owner answers one sender after the other and the first is long done before
 * the second goes on; they must not go on in lockstep. Also: each sender gets its own
 * answer. A second phase has the owner peek without sleeping between bits of work and the
 * senders arrive a little apart, so they are answered in different PeekMessage calls (KiriKiri's
 * main loop): they must not go on in lockstep either. Build: i686-w64-mingw32-gcc -O2 sendrace.c -o sendrace.exe   libs: -luser32 */
#include <stdio.h>
#include <string.h>
#include <windows.h>

#define ROUNDS 12
#define SLOTS 32

struct desc { const char *name; int size, block; char rest[92]; };
static struct desc table[SLOTS];
static const struct desc sha = { "sha256", 32, 64, "the rest of the descriptor" };
static HWND win;
static volatile LONG done;
static int slot[2], answer[2];
static HANDLE go;
static volatile int spin_sink;

static void spin(int n)
{
    for (int i = 0; i < n; i++) spin_sink += i;
}

static int find(const char *name)
{
    for (int i = 0; i < SLOTS; i++)
        if (table[i].name != NULL && strcmp(table[i].name, name) == 0) return i;
    return -1;
}

static int reg(const struct desc *d)
{
    for (int i = 0; i < SLOTS; i++)
        if (memcmp(&table[i], d, sizeof *d) == 0) return i;
    for (int i = 0; i < SLOTS; i++)
        if (table[i].name == NULL) {
            memcpy(&table[i], d, sizeof *d);
            return i;
        }
    return -1;
}

static DWORD WINAPI worker(void *arg)
{
    int k = (int)(INT_PTR)arg;
    if (go) {
        WaitForSingleObject(go, INFINITE);
        if (k) spin(20000);
    }
    answer[k] = (int)SendMessageA(win, WM_USER, 0, 100 + k);
    Sleep(0);
    int s = find("sha256");
    if (s == -1) s = reg(&sha);
    slot[k] = s;
    InterlockedIncrement(&done);
    return 0;
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_USER) return l * 2;
    return DefWindowProcA(h, m, w, l);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.lpszClassName = "sendrace";
    RegisterClassA(&wc);
    win = CreateWindowA("sendrace", "", 0, 0, 0, 10, 10, NULL, NULL, NULL, NULL);
    int bad = 0, wrong = 0;
    for (int r = 0; r < ROUNDS; r++) {
        memset(table, 0, sizeof table);
        done = 0;
        HANDLE t[2];
        for (int k = 0; k < 2; k++) t[k] = CreateThread(NULL, 0, worker, (void *)(INT_PTR)k, 0, NULL);
        /* busy, not answering: both senders wait */
        Sleep(100);
        MSG msg;
        while (done < 2) {
            while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&msg);
            Sleep(1);
        }
        WaitForMultipleObjects(2, t, TRUE, INFINITE);
        for (int k = 0; k < 2; k++) {
            CloseHandle(t[k]);
            bad += slot[k] != 0;
            wrong += answer[k] != 2 * (100 + k);
        }
    }
    printf("%d rounds: registered elsewhere than slot 0: %d, wrong answers: %d\n", ROUNDS, bad, wrong);
    /* the owner peeks between short bits of work; the senders arrive a little apart */
    go = CreateEventA(NULL, TRUE, FALSE, NULL);
    bad = wrong = 0;
    for (int r = 0; r < ROUNDS; r++) {
        memset(table, 0, sizeof table);
        done = 0;
        ResetEvent(go);
        HANDLE t[2];
        for (int k = 0; k < 2; k++) t[k] = CreateThread(NULL, 0, worker, (void *)(INT_PTR)k, 0, NULL);
        Sleep(50);
        SetEvent(go);
        MSG msg;
        while (done < 2) {
            while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&msg);
            spin(500);
        }
        WaitForMultipleObjects(2, t, TRUE, INFINITE);
        for (int k = 0; k < 2; k++) {
            CloseHandle(t[k]);
            bad += slot[k] != 0;
            wrong += answer[k] != 2 * (100 + k);
        }
    }
    printf("%d rounds, peeking owner: registered elsewhere than slot 0: %d, wrong answers: %d\n", ROUNDS, bad, wrong);
    return 0;
}
