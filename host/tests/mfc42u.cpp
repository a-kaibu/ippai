// mfc42u.dll, MFC 4.2's Unicode build (#159): mfc42.cpp's checks with TCHAR = unsigned short
// (CString UTF-16, the W functions, Unicode windows) and some of the Unicode build's own: CJK
// text and the ideographic space, an ANSI string converted, CString += a WCHAR.
#define MFCU 1
#include "mfc42.cpp"
