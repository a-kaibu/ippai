/* libs: -ladvapi32
 * A real service process, started by SCM and controlled by its parent. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static SERVICE_STATUS_HANDLE status_handle;
static SERVICE_STATUS status;
static HANDLE stopped;
static int failed;
#define CHECK(label, expr) do { int ok = !!(expr); printf("%s: %s\n", label, ok ? "ok" : "FAIL"); failed |= !ok; } while (0)
static void report(DWORD state) {
    status.dwServiceType = SERVICE_WIN32_OWN_PROCESS;
    status.dwCurrentState = state;
    status.dwControlsAccepted = state == SERVICE_RUNNING ? SERVICE_ACCEPT_STOP : 0;
    SetServiceStatus(status_handle, &status);
}
static void WINAPI control(DWORD code) {
    if (code == SERVICE_CONTROL_STOP) {
        report(SERVICE_STOP_PENDING);
        SetEvent(stopped);
    } else if (code == 128) {
        HANDLE e = OpenEventA(EVENT_MODIFY_STATE, FALSE, "sake-service-control");
        if (e) { SetEvent(e); CloseHandle(e); }
    }
}
static void WINAPI service_main(DWORD argc, LPSTR *argv) {
    status_handle = RegisterServiceCtrlHandlerA("sake-test-service", control);
    if (!status_handle) return;
    report(SERVICE_START_PENDING);
    stopped = CreateEventA(NULL, TRUE, FALSE, NULL);
    if (argc != 2 || strcmp(argv[0], "sake-test-service") || strcmp(argv[1], "argument with spaces")) {
        status.dwWin32ExitCode = ERROR_INVALID_PARAMETER;
        report(SERVICE_STOPPED);
        return;
    }
    report(SERVICE_RUNNING);
    WaitForSingleObject(stopped, 10000);
    CloseHandle(stopped);
    report(SERVICE_STOPPED);
}
static int state(SC_HANDLE svc, DWORD expected, SERVICE_STATUS_PROCESS *s) {
    DWORD n;
    for (int i = 0; i < 500; i++) {
        if (!QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, (BYTE *)s, sizeof(*s), &n)) return 0;
        if (s->dwCurrentState == expected) return 1;
        Sleep(10);
    }
    return 0;
}
int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "--service")) {
        SERVICE_TABLE_ENTRYA entries[] = {{"sake-test-service", service_main}, {NULL, NULL}};
        return StartServiceCtrlDispatcherA(entries) ? 0 : 1;
    }
    SERVICE_TABLE_ENTRYA entries[] = {{"sake-test-service", service_main}, {NULL, NULL}};
    BOOL r = StartServiceCtrlDispatcherA(entries);
    CHECK("dispatcher requires SCM", !r && GetLastError() == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT);
    SC_HANDLE manager = OpenSCManagerA(NULL, NULL, SC_MANAGER_ALL_ACCESS);
    CHECK("manager", manager != NULL);
    SC_HANDLE old = OpenServiceA(manager, "sake-test-service", SERVICE_ALL_ACCESS);
    if (old) { DeleteService(old); CloseServiceHandle(old); }
    char image[MAX_PATH], command[MAX_PATH + 32];
    GetModuleFileNameA(NULL, image, sizeof(image));
    snprintf(command, sizeof(command), "\"%s\" --service", image);
    SC_HANDLE svc = CreateServiceA(manager, "sake-test-service", "Sake test service", SERVICE_ALL_ACCESS,
        SERVICE_WIN32_OWN_PROCESS, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL, command, NULL, NULL, "RpcSs\0", NULL, NULL);
    CHECK("create", svc != NULL);
    if (!svc) return 1;
    SERVICE_STATUS_PROCESS s;
    CHECK("initially stopped", state(svc, SERVICE_STOPPED, &s) && s.dwProcessId == 0);
    HANDLE ack = CreateEventA(NULL, TRUE, FALSE, "sake-service-control");
    LPCSTR args[] = {"argument with spaces"};
    CHECK("start", StartServiceA(svc, 1, args));
    CHECK("running in child", state(svc, SERVICE_RUNNING, &s) && s.dwProcessId && s.dwProcessId != GetCurrentProcessId());
    r = StartServiceA(svc, 0, NULL);
    CHECK("reject duplicate start", !r && GetLastError() == ERROR_SERVICE_ALREADY_RUNNING);
    SERVICE_STATUS ss;
    CHECK("control delivered", ControlService(svc, 128, &ss) && WaitForSingleObject(ack, 5000) == WAIT_OBJECT_0);
    CHECK("stop", ControlService(svc, SERVICE_CONTROL_STOP, &ss));
    CHECK("stopped", state(svc, SERVICE_STOPPED, &s) && s.dwWin32ExitCode == 0);
    CHECK("restart", StartServiceA(svc, 1, args) && state(svc, SERVICE_RUNNING, &s));
    CHECK("mark delete", DeleteService(svc));
    SC_HANDLE again = OpenServiceA(manager, "sake-test-service", SERVICE_QUERY_STATUS);
    CHECK("reject marked service", !again && GetLastError() == ERROR_SERVICE_MARKED_FOR_DELETE);
    CHECK("stop marked service", ControlService(svc, SERVICE_CONTROL_STOP, &ss) && state(svc, SERVICE_STOPPED, &s));
    CHECK("close", CloseServiceHandle(svc));
    again = OpenServiceA(manager, "sake-test-service", SERVICE_QUERY_STATUS);
    CHECK("deleted after close", !again && GetLastError() == ERROR_SERVICE_DOES_NOT_EXIST);
    CloseHandle(ack);
    CloseServiceHandle(manager);
    return failed;
}
