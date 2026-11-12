/* Event-log metadata benchmark for sake's isolated Application log.
 * Build: x86_64-w64-mingw32-gcc -O2 tests/bench/eventlog.c -ladvapi32 -o eventlog-bench.exe
 * Run with a fresh SAKE_C directory: node /path/to/sake.js /path/to/eventlog-bench.exe
 * Discard round 0 (JIT warmup); alternate old/new runtimes on the same machine.
 * Each round checks the API results while timing 4000 metadata calls after 512 writes.
 * This measures metadata lookup, not game FPS. Run without competing builds.
 */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
int main(void) {
  HANDLE source = RegisterEventSourceA(NULL, "SakeWinrefProbe");
  HANDLE log = OpenEventLogA(NULL, "Application");
  HANDLE event = CreateEventA(NULL, FALSE, FALSE, NULL);
  DWORD before = 0, count = 0, oldest = 0, need = 0;
  EVENTLOG_FULL_INFORMATION info;
  if (!source || !log || !event || !GetNumberOfEventLogRecords(log, &before)) return 1;
  char payload[128] = {0};
  for (int i = 0; i < 512; ++i)
    if (!ReportEventA(source, EVENTLOG_INFORMATION_TYPE, 0, 42, NULL, 0, sizeof(payload), NULL, payload)) return 2;
  LARGE_INTEGER freq, start, end;
  QueryPerformanceFrequency(&freq);
  for (int round = 0; round < 6; ++round) {
    QueryPerformanceCounter(&start);
    for (int i = 0; i < 1000; ++i) {
      if (!GetNumberOfEventLogRecords(log, &count) || count != before + 512) return 3;
      if (!GetOldestEventLogRecord(log, &oldest) || !oldest) return 4;
      if (!GetEventLogInformation(log, EVENTLOG_FULL_INFO, &info, sizeof(info), &need) || need != sizeof(info)) return 5;
      if (!NotifyChangeEventLog(log, event)) return 6;
    }
    QueryPerformanceCounter(&end);
    printf("round %d: %.3f ms per 4000 metadata calls\n", round, 1000.0 * (end.QuadPart - start.QuadPart) / freq.QuadPart);
  }
  /* Exercise the backup-handle lookup too, outside the timed region. */
  char path[MAX_PATH];
  GetTempPathA(MAX_PATH, path);
  lstrcatA(path, "eventlog-bench.evt");
  DeleteFileA(path);
  if (!BackupEventLogA(log, path)) return 7;
  HANDLE backup = OpenBackupEventLogA(NULL, path);
  if (!backup || !GetNumberOfEventLogRecords(backup, &count) || count != before + 512) return 8;
  if (!GetOldestEventLogRecord(backup, &oldest) || !oldest) return 9;
  if (!GetEventLogInformation(backup, EVENTLOG_FULL_INFO, &info, sizeof(info), &need)) return 10;
  if (!NotifyChangeEventLog(backup, event)) return 11;
  CloseEventLog(backup); DeleteFileA(path);
  CloseHandle(event); CloseEventLog(log); DeregisterEventSource(source);
  return 0;
}
