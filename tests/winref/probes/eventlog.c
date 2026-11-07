/* advapi32's event log (eventlog.rs) as an installer or game's crash reporter uses it: the
 * parameter errors, an event source writing to the Application log, the newest record read
 * back (narrow and wide: field values and where the SID, strings and data sit), whole-record
 * reads and the needed size, seek reads, backups (made, refused over an existing file, opened,
 * the same record in them; a text file is corrupt), and StartTrace's property checks.
 * Only what does not depend on the machine's own log is printed.
 * libs: -ladvapi32 */
#include "../probe.h"
#include <evntrace.h>

#define ERR(call) do { SetLastError(0xdeadbeef); BOOL r_ = (BOOL)(INT_PTR)(call); DWORD e_ = GetLastError(); printf("%s: %d %lu\n", #call, r_ != 0, r_ ? 0 : e_); } while (0)

static const char *src = "SakeWinrefProbe";

/* The newest record of the log through h, read backwards; NULL when none */
static EVENTLOGRECORD *newest(HANDLE h, BOOL wide, DWORD *len) {
    DWORD read = 0, need = 0;
    BYTE small[sizeof(EVENTLOGRECORD)];
    BOOL r = wide ? ReadEventLogW(h, EVENTLOG_SEQUENTIAL_READ | EVENTLOG_BACKWARDS_READ, 0, small, sizeof small, &read, &need)
                  : ReadEventLogA(h, EVENTLOG_SEQUENTIAL_READ | EVENTLOG_BACKWARDS_READ, 0, small, sizeof small, &read, &need);
    DWORD e = GetLastError();
    printf("small buffer: %d %lu read %lu need>56 %d\n", r, r ? 0 : e, read, need > sizeof(EVENTLOGRECORD));
    if (r || e != ERROR_INSUFFICIENT_BUFFER) return NULL;
    EVENTLOGRECORD *rec = malloc(need);
    DWORD size = need;
    r = wide ? ReadEventLogW(h, EVENTLOG_SEQUENTIAL_READ | EVENTLOG_BACKWARDS_READ, 0, rec, size, &read, &need)
             : ReadEventLogA(h, EVENTLOG_SEQUENTIAL_READ | EVENTLOG_BACKWARDS_READ, 0, rec, size, &read, &need);
    printf("read: %d read==size %d need %lu\n", r, read == size, need);
    *len = read;
    return r ? rec : NULL;
}

static void show(EVENTLOGRECORD *r, BOOL wide) {
    BYTE *b = (BYTE *)r;
    printf("record: len==tail %d reserved %lx id %lx type %u cat %u nstr %u flags %u closing %lu time %d\n",
           r->Length == *(DWORD *)(b + r->Length - 4), r->Reserved, r->EventID, r->EventType, r->EventCategory,
           r->NumStrings, r->ReservedFlags, r->ClosingRecordNumber, r->TimeGenerated == r->TimeWritten);
    DWORD names;
    if (wide) {
        WCHAR *s = (WCHAR *)(r + 1);
        printf("source %d ", !lstrcmpW(s, L"SakeWinrefProbe"));
        names = sizeof(EVENTLOGRECORD) + (lstrlenW(s) + 1 + lstrlenW(s + lstrlenW(s) + 1) + 1) * 2;
    } else {
        char *s = (char *)(r + 1);
        printf("source %d ", !strcmp(s, src));
        names = sizeof(EVENTLOGRECORD) + lstrlenA(s) + 1 + lstrlenA(s + lstrlenA(s) + 1) + 1;
    }
    printf("sid after the names %d, at a multiple of 8 %d, len %lu, strings at sid+%lu, data at strings+%lu len %lu, end at data+%lu\n",
           r->UserSidOffset >= names && r->UserSidOffset - names < 8, r->UserSidOffset % 8 == 0, r->UserSidLength, r->StringOffset - r->UserSidOffset,
           r->DataOffset - r->StringOffset, r->DataLength, r->Length - r->DataOffset - r->DataLength);
    if (wide) {
        WCHAR *p = (WCHAR *)(b + r->StringOffset);
        for (int i = 0; i < r->NumStrings; i++, p += lstrlenW(p) + 1) printf(" [%ls]", p);
    } else {
        char *p = (char *)(b + r->StringOffset);
        for (int i = 0; i < r->NumStrings; i++, p += strlen(p) + 1) printf(" [%s]", p);
    }
    printf(" data");
    for (DWORD i = 0; i < r->DataLength; i++) printf(" %02x", b[r->DataOffset + i]);
    printf("\n");
}

int main(void) {
    DWORD n = 7;
    ERR(CloseEventLog(NULL));
    ERR(DeregisterEventSource(NULL));
    ERR(OpenEventLogA(NULL, NULL));
    ERR(OpenEventLogA("NoSuchServer", "Application"));
    ERR(RegisterEventSourceA(NULL, NULL));
    ERR(GetNumberOfEventLogRecords(NULL, NULL));
    ERR(GetNumberOfEventLogRecords(NULL, &n));
    ERR(GetOldestEventLogRecord(NULL, &n));
    ERR(BackupEventLogA(NULL, NULL));
    ERR(OpenBackupEventLogA(NULL, NULL));
    ERR(OpenBackupEventLogA(NULL, "no-such.evt"));
    ERR(ClearEventLogA(NULL, NULL));

    HANDLE s = RegisterEventSourceA(NULL, src);
    if (!s) {
        printf("skip no event source %lu\n", GetLastError());
        return 0;
    }
    ERR(ReportEventA(s, 0x20, 0, 1, NULL, 0, 0, NULL, NULL));
    ERR(ReportEventA(s, EVENTLOG_WARNING_TYPE, 0, 1, NULL, 1, 0, NULL, NULL));
    const char *strs[] = {"first string", "second"};
    BYTE data[5] = {1, 2, 3, 4, 5};
    BYTE sidbuf[SECURITY_MAX_SID_SIZE];
    DWORD sidlen = sizeof sidbuf;
    CreateWellKnownSid(WinInteractiveSid, NULL, sidbuf, &sidlen);
    ERR(ReportEventA(s, EVENTLOG_WARNING_TYPE, 3, 0x1234, sidbuf, 2, sizeof data, strs, data));
    ERR(DeregisterEventSource(s));

    HANDLE h = OpenEventLogA(NULL, "Application");
    printf("open: %d\n", h != NULL);
    BYTE info[8];
    DWORD need = 0;
    ERR(GetEventLogInformation(h, 1, info, sizeof info, &need));
    ERR(GetEventLogInformation(h, EVENTLOG_FULL_INFO, NULL, 0, &need));
    ERR(GetEventLogInformation(h, EVENTLOG_FULL_INFO, info, 2, &need));
    printf("needed %lu\n", need);
    ERR(GetEventLogInformation(h, EVENTLOG_FULL_INFO, info, sizeof info, &need));
    DWORD read;
    ERR(ReadEventLogA(h, EVENTLOG_SEQUENTIAL_READ, 0, info, sizeof info, &read, &need));
    ERR(ReadEventLogA(h, EVENTLOG_SEEK_READ | EVENTLOG_FORWARDS_READ, 0, info, sizeof info, &read, &need));
    DWORD len;
    EVENTLOGRECORD *a = newest(h, FALSE, &len);
    if (!a) return 0;
    show(a, FALSE);
    DWORD number = a->RecordNumber, count = 0, oldest = 0;
    GetNumberOfEventLogRecords(h, &count);
    GetOldestEventLogRecord(h, &oldest);
    printf("newest is oldest+count-1 %d\n", number == oldest + count - 1);
    CloseEventLog(h);

    h = OpenEventLogW(NULL, L"application");
    EVENTLOGRECORD *w = newest(h, TRUE, &len);
    if (w) show(w, TRUE);
    /* a seek read of the same record, and with room for two: the record before it comes too */
    BYTE *two = malloc(4 * len + 4096);
    BOOL r = ReadEventLogW(h, EVENTLOG_SEEK_READ | EVENTLOG_BACKWARDS_READ, number, two, 4 * len + 4096, &read, &need);
    EVENTLOGRECORD *t = (EVENTLOGRECORD *)two;
    printf("seek: %d same number %d more than one %d\n", r, t->RecordNumber == number, read > t->Length);
    if (read > t->Length) printf("second is the one before %d\n", ((EVENTLOGRECORD *)(two + t->Length))->RecordNumber == number - 1);

    char backup[MAX_PATH];
    GetTempPathA(MAX_PATH, backup);
    lstrcatA(backup, "sake-winref-eventlog.evt");
    DeleteFileA(backup);
    ERR(BackupEventLogA(h, NULL));
    ERR(BackupEventLogA(h, backup));
    ERR(BackupEventLogA(h, backup));
    printf("backup exists %d\n", GetFileAttributesA(backup) != INVALID_FILE_ATTRIBUTES);
    CloseEventLog(h);
    HANDLE b = OpenBackupEventLogA(NULL, backup), b2 = OpenBackupEventLogA("", backup);
    printf("open backup %d %d distinct %d\n", b != NULL, b2 != NULL, b != b2);
    ERR(ClearEventLogA(b, NULL));
    if (b) {
        EVENTLOGRECORD *c = newest(b, TRUE, &len);
        printf("backup has the record %d\n", c && c->RecordNumber == number && c->Length == w->Length && !memcmp(c, w, w->Length));
    }
    CloseEventLog(b);
    CloseEventLog(b2);
    DeleteFileA(backup);
    HANDLE f = CreateFileA(backup, GENERIC_WRITE, 0, NULL, CREATE_NEW, 0, NULL);
    WriteFile(f, "Just some text", 15, &read, NULL);
    CloseHandle(f);
    ERR(OpenBackupEventLogA(NULL, backup));
    DeleteFileA(backup);

    /* StartTrace's checks (only the refusals: starting one needs administrator rights) */
    struct { EVENT_TRACE_PROPERTIES p; char name[8]; char file[16]; } tp = {0};
    TRACEHANDLE th;
    tp.p.Wnode.BufferSize = sizeof tp;
    tp.p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    tp.p.LoggerNameOffset = sizeof tp.p;
    tp.p.LogFileNameOffset = sizeof tp.p + 8;
    strcpy(tp.file, "x.etl");
    printf("props size %u\n", (unsigned)sizeof tp.p);
    tp.p.Wnode.BufferSize = 0;
    printf("start: no size %lu\n", StartTraceA(&th, "probe", &tp.p));
    tp.p.Wnode.BufferSize = sizeof tp;
    printf("start: long name %lu\n", StartTraceA(&th, "a name far too long for it", &tp.p));
    printf("start: no props %lu no handle %lu\n", StartTraceA(&th, "probe", NULL), StartTraceA(NULL, "probe", &tp.p));
    tp.p.LogFileMode = EVENT_TRACE_FILE_MODE_SEQUENTIAL | EVENT_TRACE_FILE_MODE_CIRCULAR;
    printf("start: two modes %lu\n", StartTraceA(&th, "probe", &tp.p));
    tp.p.LogFileMode = 0;
    tp.p.LogFileNameOffset = 0;
    printf("start: no file %lu\n", StartTraceA(&th, "probe", &tp.p));
    return 0;
}
