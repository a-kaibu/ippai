/* WH_GETMESSAGE thread hooks (LiveMaker's live.dll sets one with WH_CALLWNDPROC and WH_CBT):
 * called for each message GetMessage / PeekMessage hands out, with PM_REMOVE or PM_NOREMOVE,
 * and what the hook changes in the MSG is what the program gets; nothing for an empty queue;
 * none after UnhookWindowsHookEx.
 * libs: -luser32 */
#include <stdio.h>
#include <windows.h>

static int calls;
static WPARAM last_flag;
static UINT last_msg;
static LRESULT CALLBACK hook(int code, WPARAM w, LPARAM l)
{
    MSG *m = (MSG *)l;
    calls++;
    last_flag = w;
    last_msg = m->message;
    if (m->message == WM_APP + 1) m->wParam = 42;
    return CallNextHookEx(NULL, code, w, l);
}

int main(void)
{
    HHOOK h = SetWindowsHookExA(WH_GETMESSAGE, hook, NULL, GetCurrentThreadId());
    printf("hook set: %d\n", h != NULL);
    MSG m;
    BOOL r;
    r = PeekMessageA(&m, NULL, 0, 0, PM_REMOVE);
    printf("empty queue: %d, calls %d\n", r, calls);

    PostThreadMessageA(GetCurrentThreadId(), WM_APP + 1, 7, 0);
    r = PeekMessageA(&m, NULL, 0, 0, PM_NOREMOVE);
    printf("PM_NOREMOVE: %d, calls %d, flag %d, msg app+%u, wParam %d\n", r, calls, (int)last_flag, last_msg - WM_APP, (int)m.wParam);
    r = PeekMessageA(&m, NULL, 0, 0, PM_REMOVE);
    printf("PM_REMOVE: %d, calls %d, flag %d, wParam %d\n", r, calls, (int)last_flag, (int)m.wParam);

    PostThreadMessageA(GetCurrentThreadId(), WM_APP + 2, 5, 0);
    r = GetMessageA(&m, NULL, 0, 0);
    printf("GetMessage: %d, calls %d, flag %d, msg app+%u, wParam %d\n", r, calls, (int)last_flag, last_msg - WM_APP, (int)m.wParam);

    printf("unhook: %d\n", UnhookWindowsHookEx(h));
    PostThreadMessageA(GetCurrentThreadId(), WM_APP + 1, 7, 0);
    r = PeekMessageA(&m, NULL, 0, 0, PM_REMOVE);
    printf("after unhook: %d, calls %d, wParam %d\n", r, calls, (int)m.wParam);
    return 0;
}
