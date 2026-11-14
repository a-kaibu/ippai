/* The keyboard layout's tables through user32: MapVirtualKey(Ex)A/W of all five MAPVK_ types,
 * ToUnicode(Ex) and ToAscii with Shift / Ctrl / Alt / CapsLock, VkKeyScan(Ex)A/W (Ren'Py's SDL
 * calls MapVirtualKeyExA and ToUnicode for every key, #146). The keys every layout has are
 * printed as they are; those whose place differs between the US and the Japanese keyboard
 * (punctuation, shifted digits) as whether they are that layout's, the one GetKeyboardLayout
 * names (the runner's is en-US; sake answers the Japanese one for code page 932). */
#include "../probe.h"

static const UINT keys[] = { 'A', 'Z', 'M', '0', '1', '9', VK_SHIFT, VK_LSHIFT, VK_RSHIFT, VK_CONTROL, VK_LCONTROL, VK_RCONTROL,
                             VK_MENU, VK_LMENU, VK_RMENU, VK_RETURN, VK_SPACE, VK_BACK, VK_TAB, VK_ESCAPE, VK_F1, VK_F10, VK_F12,
                             VK_HOME, VK_END, VK_LEFT, VK_UP, VK_RIGHT, VK_DOWN, VK_PRIOR, VK_NEXT, VK_INSERT, VK_DELETE,
                             VK_NUMPAD0, VK_NUMPAD5, VK_NUMPAD9, VK_MULTIPLY, VK_ADD, VK_SUBTRACT, VK_DECIMAL, VK_DIVIDE,
                             VK_NUMLOCK, VK_SCROLL, VK_CAPITAL, VK_LWIN, VK_APPS, 0xff };
static const UINT scans[] = { 0x01, 0x02, 0x0b, 0x0e, 0x0f, 0x10, 0x1c, 0x1d, 0x1e, 0x2a, 0x2c, 0x36, 0x38, 0x39, 0x3a,
                              0x3b, 0x45, 0x47, 0x48, 0x4c, 0x52, 0x53, 0x57, 0x58, 0xe01c, 0xe01d, 0xe035, 0xe038, 0xe047,
                              0xe048, 0xe053, 0xe05b, 0x00, 0x7f };

/* (vk, US unshifted, US shifted, JP unshifted, JP shifted, scan US, scan JP) */
static const struct { UINT vk; char un, us, jn, js; UINT su, sj; } oem[] = {
    { VK_OEM_1, ';', ':', ':', '*', 0x27, 0x28 },      { VK_OEM_PLUS, '=', '+', ';', '+', 0x0d, 0x27 },
    { VK_OEM_COMMA, ',', '<', ',', '<', 0x33, 0x33 },  { VK_OEM_MINUS, '-', '_', '-', '=', 0x0c, 0x0c },
    { VK_OEM_PERIOD, '.', '>', '.', '>', 0x34, 0x34 }, { VK_OEM_2, '/', '?', '/', '?', 0x35, 0x35 },
    { VK_OEM_3, '`', '~', '@', '`', 0x29, 0x1a },      { VK_OEM_4, '[', '{', '[', '{', 0x1a, 0x1b },
    { VK_OEM_6, ']', '}', ']', '}', 0x1b, 0x2b },      { VK_OEM_7, '\'', '"', '^', '~', 0x28, 0x0d },
    { '2', '2', '@', '2', '"', 0x03, 0x03 },           { '7', '7', '&', '7', '\'', 0x08, 0x08 },
};

static int tu(UINT vk, const BYTE *st) {
    WCHAR b[4] = { 0xcccc, 0xcccc, 0xcccc, 0xcccc };
    int n = ToUnicode(vk, MapVirtualKeyA(vk, 0), st, b, 4, 0);
    return n == 1 ? b[0] : n == 0 ? -1 : -100 - n;
}

int main(void) {
    HKL kl = GetKeyboardLayout(0);
    int ja = ((ULONG_PTR)kl & 0xffff) == 0x411;
    for (int i = 0; i < sizeof keys / sizeof keys[0]; i++) {
        UINT k = keys[i];
        printf("vk %02x: vsc %#x vsc_ex %#x char %#x W %#x ExA %#x\n", k, MapVirtualKeyA(k, MAPVK_VK_TO_VSC), MapVirtualKeyA(k, MAPVK_VK_TO_VSC_EX),
               MapVirtualKeyA(k, MAPVK_VK_TO_CHAR), MapVirtualKeyW(k, MAPVK_VK_TO_VSC), MapVirtualKeyExA(k, MAPVK_VK_TO_VSC, kl));
    }
    for (int i = 0; i < sizeof scans / sizeof scans[0]; i++) {
        UINT s = scans[i];
        printf("scan %04x: vk %#x vk_ex %#x ExW %#x\n", s, MapVirtualKeyA(s, MAPVK_VSC_TO_VK), MapVirtualKeyA(s, MAPVK_VSC_TO_VK_EX),
               MapVirtualKeyExW(s, MAPVK_VSC_TO_VK_EX, kl));
    }
    printf("type 9: %#x\n", MapVirtualKeyA('A', 9));
    BYTE none[256] = { 0 }, shift[256] = { 0 }, ctrl[256] = { 0 }, caps[256] = { 0 }, ctrlalt[256] = { 0 }, ctrlshift[256] = { 0 }, alt[256] = { 0 };
    shift[VK_SHIFT] = 0x80;
    ctrl[VK_CONTROL] = 0x80;
    caps[VK_CAPITAL] = 1;
    ctrlalt[VK_CONTROL] = ctrlalt[VK_MENU] = 0x80;
    ctrlshift[VK_CONTROL] = ctrlshift[VK_SHIFT] = 0x80;
    alt[VK_MENU] = 0x80;
    const UINT tk[] = { 'A', 'Q', '1', '5', VK_RETURN, VK_BACK, VK_TAB, VK_ESCAPE, VK_SPACE, VK_NUMPAD3, VK_MULTIPLY, VK_DIVIDE, VK_DECIMAL, VK_SHIFT, VK_F1, VK_LEFT };
    for (int i = 0; i < sizeof tk / sizeof tk[0]; i++) {
        UINT k = tk[i];
        printf("ToUnicode %02x: none %d shift %d ctrl %d caps %d ctrl+alt %d ctrl+shift %d alt %d\n", k, tu(k, none), tu(k, shift), tu(k, ctrl), tu(k, caps),
               tu(k, ctrlalt), tu(k, ctrlshift), tu(k, alt));
    }
    WORD a = 0xcccc;
    int n = ToAscii('B', 0x30, shift, &a, 0);
    printf("ToAscii B shift: %d %#x\n", n, a);
    a = 0xcccc;
    n = ToAscii(VK_F2, 0x3c, none, &a, 0);
    printf("ToAscii F2: %d\n", n);
    WCHAR wb[2] = { 0xcccc, 0xcccc };
    n = ToUnicodeEx('A', 0x1e, none, wb, 2, 0, kl);
    printf("ToUnicodeEx A: %d %#x\n", n, wb[0]);
    const char vks[] = { 'a', 'A', 'z', '0', '9', ' ', '\r', '\n', '\b', '\t', 0x1b, 0x01, 0x1a, 0x7f };
    for (int i = 0; i < sizeof vks; i++)
        printf("VkKeyScanA %#x: %#x W %#x ExA %#x\n", vks[i], (USHORT)VkKeyScanA(vks[i]), (USHORT)VkKeyScanW(vks[i]), (USHORT)VkKeyScanExA(vks[i], kl));
    printf("VkKeyScanW U+3042: %#x\n", (USHORT)VkKeyScanW(0x3042));
    printf("VkKeyScanExW U+00E9: %#x\n", (USHORT)VkKeyScanExW(0xe9, kl));
    /* the layout's own keys: whether each is this layout's (scan code, characters, VkKeyScan) */
    for (int i = 0; i < sizeof oem / sizeof oem[0]; i++) {
        char un = ja ? oem[i].jn : oem[i].un, sh = ja ? oem[i].js : oem[i].us;
        UINT sc = ja ? oem[i].sj : oem[i].su;
        UINT vk = oem[i].vk;
        printf("layout key %02x: scan %d back %d char %d unshifted %d shifted %d vkkeyscan %d\n", vk, MapVirtualKeyA(vk, 0) == sc,
               MapVirtualKeyA(sc, 1) == vk, (char)MapVirtualKeyA(vk, 2) == un, tu(vk, none) == un, tu(vk, shift) == sh,
               (USHORT)VkKeyScanA(sh) == (0x100 | vk));
    }
    return 0;
}
