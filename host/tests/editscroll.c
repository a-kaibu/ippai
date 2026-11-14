/* A multiline edit control with WS_VSCROLL keeps its vertical scroll bar as it scrolls: range 0 to
 * its last line, a page of the lines it shows, the position its first line shown. The license
 * page of せきさば！'s installer (Delphi, #222) polls GetScrollInfo on its TMemo and enables "I
 * agree" once the end shows; sake's edits did not scroll and left the scroll info at zero, so the
 * box stayed disabled. Checked here: the info after WM_SETTEXT, EM_LINESCROLL, WM_VSCROLL
 * (SB_LINEDOWN, SB_PAGEDOWN, SB_THUMBPOSITION, SB_TOP, SB_BOTTOM), EM_SCROLL, the wheel,
 * EM_GETFIRSTVISIBLELINE and EM_GETLINECOUNT, SIF_TRACKPOS (the position when no thumb is dragged:
 * the installer compares nTrackPos + nPage - 1 with nMax), and that a read-only edit takes no typing. Facts
 * that depend on the font (how many lines show) are printed as relations. Then the line and selection
 * messages as Delphi's TMemo.Lines.Add uses them (EM_LINEINDEX, EM_LINELENGTH, EM_SETSEL,
 * EM_REPLACESEL), EM_GETSEL, EM_GETLINE and EM_LINEFROMCHAR.
 * (sake: user.rs edit_scroll, edit_lines) */
#include <stdio.h>
#include <windows.h>

static HWND e;

static SCROLLINFO info(void)
{
    SCROLLINFO si = {sizeof si, SIF_ALL};
    GetScrollInfo(e, SB_VERT, &si);
    return si;
}

static int lines, page;

/* the scroll info and the first line shown, the position as wanted (-1: the last page) */
static void show(const char *what, int want)
{
    SCROLLINFO si = info();
    LRESULT first = SendMessageA(e, EM_GETFIRSTVISIBLELINE, 0, 0);
    if (want < 0)
        want = lines - page;
    printf("%-22s pos %s first is pos %d track pos is pos %d bottom %d\n", what, si.nPos == want ? "as wanted" : "OTHER", first == si.nPos,
           si.nTrackPos == si.nPos, si.nPos + (int)si.nPage == si.nMax + 1);
}

int main(void)
{
    HINSTANCE inst = GetModuleHandleA(NULL);
    HWND p = CreateWindowExA(0, "STATIC", "editscroll", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 20, 20, 400, 300, NULL, NULL, inst, NULL);
    e = CreateWindowExA(WS_EX_CLIENTEDGE, "EDIT", "", WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL,
                        10, 10, 300, 150, p, (HMENU)1, inst, NULL);
    char text[1024] = "", *o = text;
    for (int i = 1; i <= 40; i++)
        o += sprintf(o, i < 40 ? "line %d\r\n" : "line %d", i);
    SetWindowTextA(e, text);
    SCROLLINFO si = info();
    lines = SendMessageA(e, EM_GETLINECOUNT, 0, 0);
    page = si.nPage;
    printf("lines %d, min %d, max is last line %d, page some %d less than lines %d\n", lines, si.nMin, si.nMax == lines - 1, page >= 1, page < lines);
    show("SetWindowText", 0);

    printf("EM_LINESCROLL 3: %d\n", (int)SendMessageA(e, EM_LINESCROLL, 0, 3));
    show("  after", 3);
    SendMessageA(e, WM_VSCROLL, SB_LINEDOWN, 0);
    show("SB_LINEDOWN", 4);
    SendMessageA(e, WM_VSCROLL, SB_PAGEDOWN, 0);
    printf("SB_PAGEDOWN moved a page %d\n", info().nPos == 4 + page);
    SendMessageA(e, WM_VSCROLL, MAKEWPARAM(SB_THUMBPOSITION, 7), 0);
    show("SB_THUMBPOSITION 7", 7);
    SendMessageA(e, WM_VSCROLL, SB_TOP, 0);
    show("SB_TOP", 0);
    LRESULT r = SendMessageA(e, EM_SCROLL, SB_PAGEDOWN, 0);
    printf("EM_SCROLL SB_PAGEDOWN: TRUE %d moved a page %d at it %d\n", HIWORD(r), (short)LOWORD(r) == page, info().nPos == page);
    r = SendMessageA(e, EM_SCROLL, SB_LINEUP, 0);
    printf("EM_SCROLL SB_LINEUP: TRUE %d moved %d\n", HIWORD(r), (short)LOWORD(r));
    SendMessageA(e, WM_VSCROLL, SB_TOP, 0);
    r = SendMessageA(e, EM_SCROLL, SB_LINEUP, 0);
    printf("EM_SCROLL SB_LINEUP at the top: %ld\n", (long)r);

    /* the wheel: three lines a notch */
    SendMessageA(e, WM_MOUSEWHEEL, MAKEWPARAM(0, -WHEEL_DELTA), MAKELPARAM(100, 100));
    show("wheel down a notch", 3);
    SendMessageA(e, WM_MOUSEWHEEL, MAKEWPARAM(0, 2 * WHEEL_DELTA), MAKELPARAM(100, 100));
    show("wheel up two notches", 0);

    /* to the end and past it: the last page shows */
    SendMessageA(e, WM_VSCROLL, SB_BOTTOM, 0);
    show("SB_BOTTOM", -1);
    SendMessageA(e, EM_LINESCROLL, 0, 100);
    printf("EM_LINESCROLL 100: still the last page %d\n", info().nPos == lines - page);
    SendMessageA(e, EM_LINESCROLL, 0, -1000);
    show("EM_LINESCROLL -1000", 0);

    /* new text shows from its start */
    SendMessageA(e, EM_LINESCROLL, 0, 5);
    SetWindowTextA(e, text);
    show("SetWindowText again", 0);

    /* read-only: typing changes nothing */
    SendMessageA(e, WM_CHAR, 'x', 1);
    printf("read-only typed: length %d\n", GetWindowTextLengthA(e));

    /* a few lines that all show: nothing to scroll */
    SetWindowTextA(e, "one\r\ntwo");
    si = info();
    printf("short text: lines %d max %d pos %d whole %d\n", (int)SendMessageA(e, EM_GETLINECOUNT, 0, 0), si.nMax, si.nPos, (int)si.nPage > si.nMax);
    SendMessageA(e, EM_LINESCROLL, 0, 3);
    printf("  EM_LINESCROLL 3: first %d\n", (int)SendMessageA(e, EM_GETFIRSTVISIBLELINE, 0, 0));

    /* Delphi's TMemo.Lines.Add: the line count (less an empty last line), EM_LINEINDEX of the new
     * line, EM_SETSEL there, EM_REPLACESEL with the line and a line break (the confirmation page
     * of the same installer filled its memo so and showed it empty) */
    HWND m = CreateWindowExA(0, "EDIT", "", WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE, 10, 170, 300, 100, p, (HMENU)2, inst, NULL);
    static const char *add[] = {"first", "second", "", "fourth"};
    for (int i = 0; i < 4; i++)
    {
        int count = (int)SendMessageA(m, EM_GETLINECOUNT, 0, 0);
        if (SendMessageA(m, EM_LINELENGTH, SendMessageA(m, EM_LINEINDEX, count - 1, 0), 0) == 0)
            count--;
        LRESULT at = SendMessageA(m, EM_LINEINDEX, count, 0);
        char line[32];
        sprintf(line, "%s\r\n", add[i]);
        SendMessageA(m, EM_SETSEL, at, at);
        SendMessageA(m, EM_REPLACESEL, 0, (LPARAM)line);
        DWORD a, b;
        SendMessageA(m, EM_GETSEL, (WPARAM)&a, (LPARAM)&b);
        printf("Lines.Add %d: count before %d, at %d, selection %lu-%lu\n", i, count, (int)at, a, b);
    }
    char all[128];
    GetWindowTextA(m, all, sizeof all);
    for (char *q = all; *q; q++)
        if (*q == '\r' || *q == '\n')
            *q = *q == '\r' ? '|' : '/';
    printf("memo text: %s (lines %d)\n", all, (int)SendMessageA(m, EM_GETLINECOUNT, 0, 0));
    for (int i = 0; i < 6; i++)
    {
        char buf[16] = {0};
        *(WORD *)buf = sizeof buf - 1;
        LRESULT n = SendMessageA(m, EM_GETLINE, i, (LPARAM)buf);
        buf[n] = 0;
        LRESULT at = SendMessageA(m, EM_LINEINDEX, i, 0);
        printf("  line %d: index %d length %d EM_GETLINE %d \"%s\" from char %d\n", i, (int)at, (int)SendMessageA(m, EM_LINELENGTH, at, 0), (int)n, buf,
               (int)SendMessageA(m, EM_LINEFROMCHAR, at, 0));
    }
    /* a selection to replace, both ways round, and the end past the text */
    SendMessageA(m, EM_SETSEL, 7, 0);
    SendMessageA(m, EM_REPLACESEL, 0, (LPARAM)"1st");
    GetWindowTextA(m, all, 12);
    DWORD a, b;
    SendMessageA(m, EM_GETSEL, (WPARAM)&a, (LPARAM)&b);
    printf("replaced 7-0: \"%.9s\" caret %lu-%lu\n", all, a, b);
    SendMessageA(m, EM_SETSEL, 0, -1);
    SendMessageA(m, EM_GETSEL, (WPARAM)&a, (LPARAM)&b);
    printf("select all: %lu-%lu of %d\n", a, b, GetWindowTextLengthA(m));
    SendMessageA(m, EM_SETSEL, -1, 0);
    SendMessageA(m, EM_GETSEL, (WPARAM)&a, (LPARAM)&b);
    printf("deselect: %lu-%lu\n", a, b);
    DestroyWindow(p);
    return 0;
}
