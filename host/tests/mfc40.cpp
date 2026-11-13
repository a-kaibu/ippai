// mfc40.dll, MFC 4.0 as Visual C++ 4 programs use it (#159): mfc42.cpp's checks on its
// ordinals and layouts (CWinThread 0x64 bytes, CWinApp's fields 8 lower than MFC 4.2's).
#define MFC40 1
#include "mfc42.cpp"
