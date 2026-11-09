/* A launcher chain like Key's (anemoi): Start.exe opens StartData\StartMenu.exe with
 * ShellExecuteExW (/from="autorun", no directory, no wait) and exits; the menu reads Start.ini
 * beside it and opens GameData\SiglusEngine.exe the same way (/window). Each prints what it
 * saw, so a program started without its game's files says so. Built three times:
 *   STAGE=0 Start.exe, STAGE=1 StartData\StartMenu.exe, STAGE=2 StartData\GameData\SiglusEngine.exe
 * (tests/exec-browser.mjs: the page that takes over keeps the game's mounts) */
#include <windows.h>
#include <shellapi.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

#ifndef STAGE
#define STAGE 0
#endif

static int open_next(const wchar_t *dir, const wchar_t *rel, const wchar_t *args) {
  wchar_t path[MAX_PATH];
  swprintf(path, MAX_PATH, L"%ls\\%ls", dir, rel);
  DWORD a = GetFileAttributesW(path);
  if (a == INVALID_FILE_ATTRIBUTES || (a & FILE_ATTRIBUTE_DIRECTORY)) {
    printf("chain: %ls not found\n", rel);
    return 1;
  }
  SHELLEXECUTEINFOW sei = {sizeof sei};
  sei.fMask = SEE_MASK_NOCLOSEPROCESS;
  sei.lpVerb = L"open";
  sei.lpFile = path;
  sei.lpParameters = args;
  sei.nShow = SW_SHOW;
  if (!ShellExecuteExW(&sei)) {
    printf("chain: ShellExecuteExW %ls failed %lu\n", rel, GetLastError());
    return 1;
  }
  return 0;
}

int main(void) {
  wchar_t dir[MAX_PATH];
  GetModuleFileNameW(NULL, dir, MAX_PATH);
  *wcsrchr(dir, L'\\') = 0;
  SetCurrentDirectoryW(dir);
  const wchar_t *cmd = GetCommandLineW();
  const wchar_t *args = cmd[0] == L'"' ? wcschr(cmd + 1, L'"') + 1 : wcschr(cmd, L' ');
  if (!args) args = L"";
  while (*args == L' ') args++;
#if STAGE == 0
  printf("chain 0: start\n");
  return open_next(dir, L"StartData\\StartMenu.exe", L"/from=\"autorun\"");
#elif STAGE == 1
  char ini[64] = "";
  FILE *f = fopen("Start.ini", "rb");
  if (f) {
    if (!fgets(ini, sizeof ini, f)) ini[0] = 0;
    fclose(f);
  }
  ini[strcspn(ini, "\r\n")] = 0;
  printf("chain 1: args [%ls] ini [%s]\n", args, ini);
  return open_next(dir, L"GameData\\SiglusEngine.exe", L"/window");
#else
  printf("chain 2: args [%ls] gamedata %s\n", args, GetFileAttributesW(L"Gameexe.dat") != INVALID_FILE_ATTRIBUTES ? "yes" : "no");
  printf("end\n");
  return 0;
#endif
}
