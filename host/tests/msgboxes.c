/* libs: -luser32 */
/* The message boxes installers and games show, all four entry points with every button set:
 * sake shows none and answers with the button that goes on (OK, Ignore, Yes, Cancel,
 * Continue), so this is not compared with Windows by tests/winref (a box waits for a click
 * there). NSIS installers ask through MessageBoxIndirectW, which was unimplemented (returned 0).
 * (sake: user.rs box_answer, msgboxparams) */
#include <windows.h>
#include <stdio.h>

int main(void) {
    for (UINT t = 0; t <= 6; t++) {
        UINT style = t | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_TOPMOST;
        MSGBOXPARAMSW pw = { sizeof pw, NULL, NULL, L"本文", L"題", style, NULL, 0, NULL, 0 };
        MSGBOXPARAMSA pa = { sizeof pa, NULL, NULL, "text", "caption", style, NULL, 0, NULL, 0 };
        printf("type %u: A %d W %d ExA %d ExW %d IndirectA %d IndirectW %d\n", t,
               MessageBoxA(NULL, "text", "caption", style), MessageBoxW(NULL, L"本文", L"題", style),
               MessageBoxExA(NULL, "text", "caption", style, 0), MessageBoxExW(NULL, L"本文", L"題", style, 0),
               MessageBoxIndirectA(&pa), MessageBoxIndirectW(&pw));
    }
    /* text and caption as resource ids (hInstance given) */
    MSGBOXPARAMSW ids = { sizeof ids, NULL, GetModuleHandleW(NULL), MAKEINTRESOURCEW(7), MAKEINTRESOURCEW(8), MB_YESNO, NULL, 0, NULL, 0 };
    printf("resource ids: %d\n", MessageBoxIndirectW(&ids));
    return 0;
}
