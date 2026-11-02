/* Japanese text metrics through GDI: a private OpenType (CFF) font as SiglusEngine loads it,
 * then per-character GetGlyphOutlineW (GGO_GRAY8) cell advances, GetTextExtentPoint32W,
 * GetCharABCWidthsW and the A functions with Shift-JIS. Prints the numbers; "ok" when every
 * full-width character of the fixed-pitch faces advances by the em and the pen walks without overlap.
 * Build: i686-w64-mingw32-gcc -O2 fonttest.c -o fonttest.exe -lgdi32 */
#include <windows.h>
#include <stdio.h>

static int bad;

static void check(const char *what, int got, int want) {
  if (got != want) {
    printf("  BAD %s: %d (want %d)\n", what, got, want);
    bad++;
  }
}

static void run(const wchar_t *face, int px, int full) {
  HDC dc = CreateCompatibleDC(0);
  HFONT f = CreateFontW(-px, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, face);
  SelectObject(dc, f);
  wchar_t got[64];
  GetTextFaceW(dc, 64, got);
  TEXTMETRICW tm;
  GetTextMetricsW(dc, &tm);
  printf("%ls %d -> %ls: height %ld ascent %ld descent %ld ave %ld max %ld charset %d\n", face, px, got, tm.tmHeight,
         tm.tmAscent, tm.tmDescent, tm.tmAveCharWidth, tm.tmMaxCharWidth, tm.tmCharSet);
  const wchar_t *s = L"あいう漢字テスト、。「」ABCabc";
  MAT2 m = {{0, 1}, {0, 0}, {0, 0}, {0, 1}};
  int pen = 0;
  for (const wchar_t *p = s; *p; p++) {
    GLYPHMETRICS gm;
    DWORD n = GetGlyphOutlineW(dc, *p, GGO_GRAY8_BITMAP, &gm, 0, 0, &m);
    ABC abc;
    GetCharABCWidthsW(dc, *p, *p, &abc);
    SIZE sz;
    GetTextExtentPoint32W(dc, p, 1, &sz);
    printf("  U+%04X cell %d box %ux%u origin %ld,%ld abc %d/%u/%d extent %ld size %lu\n", *p, gm.gmCellIncX,
           gm.gmBlackBoxX, gm.gmBlackBoxY, gm.gmptGlyphOrigin.x, gm.gmptGlyphOrigin.y, abc.abcA, abc.abcB, abc.abcC,
           sz.cx, n);
    if (full && *p >= 0x3000) {
      check("cell", gm.gmCellIncX, px);
      check("extent", sz.cx, px);
      check("abc", abc.abcA + (int)abc.abcB + abc.abcC, px);
    }
    if (gm.gmptGlyphOrigin.x < -1 || (int)gm.gmBlackBoxX > gm.gmCellIncX + 2)
      printf("  BAD box outside the cell\n"), bad++;
    pen += gm.gmCellIncX;
  }
  SIZE all;
  GetTextExtentPoint32W(dc, s, lstrlenW(s), &all);
  check("whole extent", all.cx, pen);
  /* the A function with Shift-JIS: "漢" is 0x8ABF */
  GLYPHMETRICS gm;
  GetGlyphOutlineA(dc, 0x8ABF, GGO_METRICS, &gm, 0, 0, &m);
  check("A cell (0x8ABF)", gm.gmCellIncX, px);
  SIZE sa;
  GetTextExtentPoint32A(dc, "\x8a\xbf\x8e\x9a", 4, &sa);
  check("A extent (2 kanji)", sa.cx, 2 * px);
  DeleteObject(f);
  DeleteDC(dc);
}

int main(int argc, char **argv) {
  for (int i = 1; i < argc; i++) {
    wchar_t w[MAX_PATH];
    MultiByteToWideChar(CP_ACP, 0, argv[i], -1, w, MAX_PATH);
    printf("AddFontResourceExW(%s): %d\n", argv[i], AddFontResourceExW(w, FR_PRIVATE, 0));
  }
  run(L"Noto Sans CJK JP DemiLight", 32, 1);
  run(L"FOT-Chiaro Std", 26, 1);
  run(L"ＭＳ ゴシック", 24, 1);
  run(L"ＭＳ Ｐゴシック", 24, 0); /* (proportional kana) */
  printf(bad ? "%d bad\n" : "ok\n", bad);
  return 0;
}
