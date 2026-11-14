/* Command lines split into arguments: CommandLineToArgvW on quotes, backslashes and spaces
 * (the documented rules and the "" inside quotes one), and what a child started with
 * lpApplicationName and a command line that leaves the program out sees: Visual Studio's MSI
 * loader (setup.exe of 揺れるバスガイド, msiloadr) runs CreateProcess("...\\msiexec.exe",
 * " /i \"x.msi\"") and msiexec reads "/i" as its first argument. The child writes its
 * GetCommandLine, CommandLineToArgvW of it and the C runtime's __argc/__argv to a file the
 * parent prints. Runs in the current directory (wr-argv.txt).
 * libs: -lshell32 */
#include "../probe.h"
#include <shellapi.h>

static void split(const wchar_t *s) {
    int n = -1;
    wchar_t **a = CommandLineToArgvW(s, &n);
    printf("[%ls] -> %d:", s, n);
    for (int i = 0; a && i < n; i++) printf(" <%ls>", a[i]);
    printf("\n");
    LocalFree(a);
}

static int child(void) {
    FILE *f = fopen("wr-argv.txt", "w");
    if (!f) return 2;
    fprintf(f, "child GetCommandLineA [%s]\n", GetCommandLineA());
    int n = -1;
    wchar_t **a = CommandLineToArgvW(GetCommandLineW(), &n);
    fprintf(f, "child CommandLineToArgvW %d:", n);
    for (int i = 1; a && i < n; i++) fprintf(f, " <%ls>", a[i]);
    fprintf(f, "\nchild argv[0] empty %d\n", a && n > 0 && !a[0][0]);
    LocalFree(a);
    fprintf(f, "child __argc %d:", __argc);
    for (int i = 1; i < __argc; i++) fprintf(f, " <%s>", __argv[i]);
    fprintf(f, "\n");
    fclose(f);
    return 0;
}

static void run(const char *cmd) {
    char self[MAX_PATH], line[512];
    GetModuleFileNameA(NULL, self, MAX_PATH);
    strcpy(line, cmd);
    DeleteFileA("wr-argv.txt");
    STARTUPINFOA si = { sizeof si };
    PROCESS_INFORMATION pi;
    SetEnvironmentVariableA("WR_ARGV_CHILD", "1");
    BOOL ok = CreateProcessA(self, line, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi);
    SetEnvironmentVariableA("WR_ARGV_CHILD", NULL);
    printf("CreateProcess(self, [%s]) = %d\n", cmd, ok);
    if (!ok) return;
    WaitForSingleObject(pi.hProcess, 20000);
    DWORD code = 99;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    printf("exit %lu\n", code);
    FILE *f = fopen("wr-argv.txt", "r");
    char buf[1024];
    while (f && fgets(buf, sizeof buf, f)) fputs(buf, stdout);
    if (f) fclose(f);
    DeleteFileA("wr-argv.txt");
}

int main(void) {
    if (GetEnvironmentVariableA("WR_ARGV_CHILD", NULL, 0)) return child();
    split(L"p a b");
    split(L" /i x.msi");
    split(L"\t/i x.msi");
    split(L"\"C:\\a b\\p.exe\" one \"two three\"");
    split(L"\"C:\\a b\\p.exe\"tail x");
    split(L"p \"a b\" c");
    split(L"p a\\\\b \"c\\\\\" d");
    split(L"p a\\\"b c");
    split(L"p a\\\\\\\"b c");
    split(L"p a\\\\\"b c\" d");
    split(L"p \"\" x");
    split(L"p \"a\"\"b\" c");
    split(L"p \"a\"\"\"b c");
    split(L"p a\"b\"c");
    split(L"p    a\t\tb   ");
    split(L"p \"unterminated arg");
    split(L"p NAME=\"C:\\My Game\" X=1");
    run(" /i \"x y.msi\" /qb");
    run("prog /i x.msi");
    run("\"quoted prog\" a \"b c\"");
    return 0;
}
