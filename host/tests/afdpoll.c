/* libs: -lws2_32 -lntdll */
/* \Device\Afd's poll as Rust's mio (tokio) waits for sockets (the Edicius trial quit at start
 * when ntdll had no NtDeviceIoControlFile / RtlNtStatusToDosError): NtCreateFile of
 * "\Device\Afd\Mio" tied to a completion port, IOCTL_AFD_POLL per socket. A poll whose event is
 * not true yet stays pending and its packet (ApcContext as the OVERLAPPED value) comes when a
 * send makes the socket readable; SEND is true at once on a connected socket; NtCancelIoFileEx
 * ends a poll as cancelled; closing the polled socket answers LOCAL_CLOSE; a listening socket
 * reports ACCEPT; the peer's close DISCONNECT. Then the other ntdll functions that runtime
 * asks for: RtlNtStatusToDosError of a few codes, NtQueryVolumeInformationFile of a file
 * (device type, file system name, a too-small buffer). */
#include <winsock2.h>
#include <windows.h>
#include <winternl.h>
#include <stdio.h>

typedef struct { LARGE_INTEGER Timeout; ULONG NumberOfHandles; ULONG Exclusive; struct { HANDLE Handle; ULONG Events; LONG Status; } Handles[1]; } AFD_POLL_INFO;
NTSTATUS NTAPI NtCancelIoFileEx(HANDLE, PIO_STATUS_BLOCK, PIO_STATUS_BLOCK);
ULONG NTAPI RtlNtStatusToDosError(NTSTATUS);

#define IOCTL_AFD_POLL 0x00012024
#define AFD_RECEIVE 0x1
#define AFD_SEND 0x4
#define AFD_DISCONNECT 0x8
#define AFD_ABORT 0x10
#define AFD_LOCAL_CLOSE 0x20
#define AFD_ACCEPT 0x80
#define ALL (AFD_RECEIVE | AFD_DISCONNECT | AFD_ABORT | AFD_LOCAL_CLOSE | AFD_ACCEPT)

static HANDLE afd, port;
static IO_STATUS_BLOCK iosb;
static AFD_POLL_INFO in, out;

static NTSTATUS poll(SOCKET s, ULONG events) {
    in.Timeout.QuadPart = 0x7fffffffffffffffLL;
    in.NumberOfHandles = 1;
    in.Exclusive = 0;
    in.Handles[0].Handle = (HANDLE)s;
    in.Handles[0].Events = events;
    in.Handles[0].Status = 0;
    memset(&out, 0, sizeof out);
    iosb.Status = -1;
    return NtDeviceIoControlFile(afd, NULL, NULL, &iosb, &iosb, IOCTL_AFD_POLL, &in, sizeof in, &out, sizeof out);
}

static void packet(const char *what, DWORD ms) {
    DWORD n = 0;
    ULONG_PTR key = 0;
    OVERLAPPED *ov = NULL;
    BOOL ok = GetQueuedCompletionStatus(port, &n, &key, &ov, ms);
    if (!ok && !ov) {
        printf("%s: no packet (%lu)\n", what, GetLastError());
        return;
    }
    printf("%s: packet ok %d key %lu overlapped %s status %08lx bytes %s handles %lu events %#lx same socket %d\n", what, ok,
           (unsigned long)key, ov == (OVERLAPPED *)&iosb ? "iosb" : "other", (unsigned long)iosb.Status,
           n == FIELD_OFFSET(AFD_POLL_INFO, Handles[1]) ? "one handle" : n == 0 ? "0" : "?", out.NumberOfHandles, out.Handles[0].Events, out.Handles[0].Handle == in.Handles[0].Handle);
}

int main(void) {
    WSADATA wd;
    WSAStartup(MAKEWORD(2, 2), &wd);
    UNICODE_STRING name;
    RtlInitUnicodeString(&name, L"\\Device\\Afd\\Mio");
    OBJECT_ATTRIBUTES oa = {0};
    oa.Length = sizeof oa;
    oa.ObjectName = &name;
    IO_STATUS_BLOCK cio;
    NTSTATUS st = NtCreateFile(&afd, SYNCHRONIZE, &oa, &cio, NULL, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_OPEN, 0, NULL, 0);
    printf("NtCreateFile \\Device\\Afd\\Mio: %08lx\n", (unsigned long)st);
    port = CreateIoCompletionPort(afd, NULL, 7, 0);
    printf("port: %d, skip modes: %d\n", port != NULL, SetFileCompletionNotificationModes(afd, FILE_SKIP_SET_EVENT_ON_HANDLE));

    SOCKET l = socket(AF_INET, SOCK_STREAM, 0), c = socket(AF_INET, SOCK_STREAM, 0), a;
    struct sockaddr_in sa = {0};
    int len = sizeof sa;
    sa.sin_family = AF_INET;
    sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(l, (struct sockaddr *)&sa, sizeof sa);
    listen(l, 4);
    getsockname(l, (struct sockaddr *)&sa, &len);
    printf("listening poll: %08lx\n", (unsigned long)poll(l, ALL));
    connect(c, (struct sockaddr *)&sa, sizeof sa);
    packet("after connect", 2000);
    a = accept(l, NULL, NULL);

    st = poll(a, ALL);
    printf("receive poll (nothing sent): %08lx, iosb %08lx\n", (unsigned long)st, (unsigned long)iosb.Status);
    packet("before send", 50);
    send(c, "hi", 2, 0);
    packet("after send", 2000);

    /* (true at once: STATUS_SUCCESS or STATUS_PENDING, a packet either way) */
    st = poll(a, AFD_SEND);
    printf("send poll: %s\n", st == 0 || st == 0x103 ? "accepted" : "failed");
    packet("send", 2000);

    char buf[8];
    recv(a, buf, sizeof buf, 0);
    printf("poll to cancel: %08lx\n", (unsigned long)poll(a, AFD_RECEIVE));
    IO_STATUS_BLOCK cs;
    printf("NtCancelIoFileEx: %08lx\n", (unsigned long)NtCancelIoFileEx(afd, &iosb, &cs));
    packet("cancelled", 2000);
    printf("NtCancelIoFileEx again: %08lx\n", (unsigned long)NtCancelIoFileEx(afd, &iosb, &cs));

    st = poll(a, ALL);
    printf("disconnect poll: %08lx\n", (unsigned long)st);
    closesocket(c);
    packet("peer closed", 2000);

    st = poll(a, AFD_RECEIVE);
    printf("local close poll: %s\n", st == 0 || st == 0x103 ? "accepted" : "failed");
    closesocket(a);
    packet("closed", 2000);

    closesocket(l);
    CloseHandle(afd);
    printf("RtlNtStatusToDosError: %lu %lu %lu %lu %lu\n", RtlNtStatusToDosError(0xC0000034), RtlNtStatusToDosError(0xC0000120),
           RtlNtStatusToDosError(0x80070005), RtlNtStatusToDosError(0x103), RtlNtStatusToDosError(0xC0000005));
    char exe[MAX_PATH];
    GetModuleFileNameA(NULL, exe, MAX_PATH);
    HANDLE f = CreateFileA(exe, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    struct { ULONG type, chars; } dev = {0};
    IO_STATUS_BLOCK io;
    st = NtQueryVolumeInformationFile(f, &io, &dev, sizeof dev, FileFsDeviceInformation);
    printf("device: %08lx type %lu chars %lu info %lu\n", (unsigned long)st, dev.type, dev.chars, (unsigned long)io.Information);
    struct { ULONG attrs; LONG maxname; ULONG len; WCHAR name[16]; } fa = {0};
    st = NtQueryVolumeInformationFile(f, &io, &fa, sizeof fa, FileFsAttributeInformation);
    printf("attributes: %08lx name %.*ls max %ld info %lu\n", (unsigned long)st, (int)(fa.len / 2), fa.name, fa.maxname, (unsigned long)io.Information);
    st = NtQueryVolumeInformationFile(f, &io, &fa, 8, FileFsAttributeInformation);
    printf("attributes in 8 bytes: %08lx\n", (unsigned long)st);
    st = NtQueryVolumeInformationFile(f, &io, &fa, 14, FileFsAttributeInformation);
    printf("attributes in 14 bytes: %08lx info %lu\n", (unsigned long)st, (unsigned long)io.Information);
    CloseHandle(f);
    return 0;
}
