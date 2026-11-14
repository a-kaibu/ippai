/* Controls of a superclass: a class registered with GetClassInfo("Button")'s (or "Edit"'s)
 * window procedure under a name of its own, its procedure handing the messages on to the
 * system one. Delphi's VCL makes every TButton, TCheckBox and TEdit so (CreateSubClass); on
 * Windows they are buttons and edits like any other. sake chose a control's behaviour by its
 * class name, so a click on せきさば！'s installer's TCheckBox/TButton did nothing (#222).
 * Clicks are WM_LBUTTONDOWN/UP sent to the control, as the system's input would deliver them;
 * the parent logs the WM_COMMAND notifications. Also the check states per button style (BS_3STATE
 * and BS_CHECKBOX leave the state to the program, BS_AUTO3STATE cycles three), a release
 * outside the button (no click), BM_CLICK, and characters typed into an edit superclass.
 * (sake: user.rs control_proc / Base, ctl_click) */
#include <stdio.h>
#include <windows.h>

static WNDPROC button_proc, edit_proc;
static char log_[256];
static int n;

static LRESULT CALLBACK super_button(HWND h, UINT m, WPARAM wp, LPARAM lp)
{
    return CallWindowProcA(button_proc, h, m, wp, lp);
}

static LRESULT CALLBACK super_edit(HWND h, UINT m, WPARAM wp, LPARAM lp)
{
    return CallWindowProcA(edit_proc, h, m, wp, lp);
}

static LRESULT CALLBACK parent_proc(HWND h, UINT m, WPARAM wp, LPARAM lp)
{
    if (m == WM_COMMAND && HIWORD(wp) == BN_CLICKED && n + 16 < (int)sizeof(log_))
        n += sprintf(log_ + n, "clicked %u ", LOWORD(wp));
    return DefWindowProcA(h, m, wp, lp);
}

static const char *took(void)
{
    static char b[256];
    strcpy(b, n ? log_ : "(none)");
    n = 0;
    log_[0] = 0;
    return b;
}

static void click(HWND b, int x, int y)
{
    SendMessageA(b, WM_LBUTTONDOWN, MK_LBUTTON, MAKELPARAM(5, 5));
    SendMessageA(b, WM_LBUTTONUP, 0, MAKELPARAM(x, y));
}

int main(void)
{
    HINSTANCE inst = GetModuleHandleA(NULL);
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = parent_proc;
    wc.hInstance = inst;
    wc.lpszClassName = "superparent";
    RegisterClassA(&wc);

    /* the superclasses, as VCL's TWinControl.CreateSubClass makes them */
    WNDCLASSA bc, ec;
    printf("GetClassInfo Button %d Edit %d\n", GetClassInfoA(NULL, "Button", &bc) != 0, GetClassInfoA(NULL, "Edit", &ec) != 0);
    button_proc = bc.lpfnWndProc;
    edit_proc = ec.lpfnWndProc;
    bc.lpfnWndProc = super_button;
    bc.hInstance = inst;
    bc.lpszClassName = "TCheckBox";
    ec.lpfnWndProc = super_edit;
    ec.hInstance = inst;
    ec.lpszClassName = "TEdit";
    printf("RegisterClass TCheckBox %d TEdit %d\n", RegisterClassA(&bc) != 0, RegisterClassA(&ec) != 0);

    HWND p = CreateWindowExA(0, "superparent", "superclass", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 20, 20, 400, 300, NULL, NULL, inst, NULL);
    static const struct { const char *name; DWORD style; } styles[] = {
        {"BS_PUSHBUTTON", BS_PUSHBUTTON}, {"BS_CHECKBOX", BS_CHECKBOX}, {"BS_AUTOCHECKBOX", BS_AUTOCHECKBOX},
        {"BS_3STATE", BS_3STATE}, {"BS_AUTO3STATE", BS_AUTO3STATE},
    };
    for (int super = 0; super < 2; super++)
    {
        const char *cls = super ? "TCheckBox" : "Button";
        for (int k = 0; k < (int)(sizeof(styles) / sizeof(styles[0])); k++)
        {
            UINT id = 100 * (super + 1) + k;
            HWND b = CreateWindowExA(0, cls, "b", WS_CHILD | WS_VISIBLE | styles[k].style, 10, 10 + 25 * k, 120, 20, p, (HMENU)(UINT_PTR)id, inst, NULL);
            printf("%s %s:", cls, styles[k].name);
            for (int i = 0; i < 3; i++)
            {
                click(b, 5, 5);
                printf(" [%s check %ld]", took(), (long)SendMessageA(b, BM_GETCHECK, 0, 0));
            }
            printf("\n");
            /* the button released outside it: no click */
            click(b, -5, -5);
            printf("  released outside: %s, capture none %d\n", took(), GetCapture() == NULL);
            SendMessageA(b, BM_CLICK, 0, 0);
            printf("  BM_CLICK: %s check %ld\n", took(), (long)SendMessageA(b, BM_GETCHECK, 0, 0));
            DestroyWindow(b);
        }
    }

    /* two auto radio buttons of the superclass: a click checks one and clears the other */
    HWND r1 = CreateWindowExA(0, "TCheckBox", "r1", WS_CHILD | WS_VISIBLE | BS_AUTORADIOBUTTON | WS_GROUP, 10, 10, 120, 20, p, (HMENU)301, inst, NULL);
    HWND r2 = CreateWindowExA(0, "TCheckBox", "r2", WS_CHILD | WS_VISIBLE | BS_AUTORADIOBUTTON, 10, 35, 120, 20, p, (HMENU)302, inst, NULL);
    click(r1, 5, 5);
    printf("radio r1: %s r1 %ld r2 %ld\n", took(), (long)SendMessageA(r1, BM_GETCHECK, 0, 0), (long)SendMessageA(r2, BM_GETCHECK, 0, 0));
    click(r2, 5, 5);
    printf("radio r2: %s r1 %ld r2 %ld\n", took(), (long)SendMessageA(r1, BM_GETCHECK, 0, 0), (long)SendMessageA(r2, BM_GETCHECK, 0, 0));

    /* characters typed into an edit of the superclass */
    HWND e = CreateWindowExA(0, "TEdit", "", WS_CHILD | WS_VISIBLE | ES_AUTOHSCROLL, 10, 70, 120, 20, p, (HMENU)400, inst, NULL);
    SendMessageA(e, WM_CHAR, 'a', 1);
    SendMessageA(e, WM_CHAR, 'b', 1);
    char t[32] = "";
    GetWindowTextA(e, t, sizeof t);
    printf("TEdit typed: \"%s\"\n", t);
    DestroyWindow(p);
    return 0;
}
