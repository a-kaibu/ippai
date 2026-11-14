/* keybd_event and mouse_event, SendInput's older forms: 催●学級 体験版 (ggs) lets go of every key
 * GetKeyboardState shows down with keybd_event(vk, scan, KEYEVENTF_KEYUP) once its launcher
 * dialog closes; sake had neither function. The checks look only at what is the same on any
 * desktop: the asynchronous key state of a key nobody holds (F13), and the cursor that an
 * absolute mouse_event move puts in the middle of the screen.
 * libs: -luser32 */
#include <stdio.h>
#include <windows.h>

static int down(int vk) { return (GetAsyncKeyState(vk) & 0x8000) != 0; }

int main(void)
{
    printf("F13 at first: %d\n", down(VK_F13));
    keybd_event(VK_F13, (BYTE)MapVirtualKeyA(VK_F13, 0), 0, 0);
    printf("F13 after keybd_event down: %d\n", down(VK_F13));
    BYTE keys[256];
    GetKeyboardState(keys);
    /* (the game's loop: every key the thread's state shows down, let go) */
    keybd_event(VK_F13, (BYTE)MapVirtualKeyA(VK_F13, 0), KEYEVENTF_KEYUP, 0);
    printf("F13 after keybd_event up: %d\n", down(VK_F13));

    /* a second key down and up through SendInput, for the same answers */
    INPUT in[2] = {0};
    in[0].type = in[1].type = INPUT_KEYBOARD;
    in[0].ki.wVk = in[1].ki.wVk = VK_F14;
    in[1].ki.dwFlags = KEYEVENTF_KEYUP;
    UINT n = SendInput(1, in, sizeof(INPUT));
    printf("SendInput F14 down: %u, down %d\n", n, down(VK_F14));
    n = SendInput(1, in + 1, sizeof(INPUT));
    printf("SendInput F14 up: %u, down %d\n", n, down(VK_F14));

    /* the middle of the primary screen: 32768 of 65536 */
    int w = GetSystemMetrics(SM_CXSCREEN), h = GetSystemMetrics(SM_CYSCREEN);
    mouse_event(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, 32768, 32768, 0, 0);
    Sleep(50);
    POINT p;
    GetCursorPos(&p);
    printf("absolute move to the middle: %d %d\n", abs(p.x - w / 2) <= 1, abs(p.y - h / 2) <= 1);
    mouse_event(MOUSEEVENTF_MOVE, 10, 5, 0, 0);
    Sleep(50);
    POINT q;
    GetCursorPos(&q);
    /* (relative moves go through the pointer speed and acceleration: only the direction counts) */
    printf("relative move right and down: %d %d\n", q.x > p.x, q.y > p.y);
    return 0;
}
