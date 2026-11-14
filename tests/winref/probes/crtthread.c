/* msvcrt's per-thread state (crt.rs): strtok's context, rand's seed and localtime's buffer
 * belong to each thread. A second thread tokenising and drawing while the first is halfway
 * through: what each gets. */
#include "../probe.h"
#include <time.h>

static char *sp = " ";
static DWORD WINAPI other(void *arg) {
    char x[] = "x y";
    char *none = strtok(NULL, sp);
    printf("other: strtok(NULL) first %s\n", none ? none : "(null)");
    char *t1 = strtok(x, sp);
    char *t2 = strtok(NULL, sp);
    printf("other: tokens %s %s\n", t1, t2);
    int r1 = rand(), r2 = rand();
    printf("other: rand %d %d\n", r1, r2);
    time_t t = 86400 * 3;
    struct tm *tm = gmtime(&t);
    printf("other: gmtime mday %d, same buffer as the first thread's: %d\n", tm->tm_mday, tm == (struct tm *)arg);
    return 0;
}

int main(void) {
    char a[] = "a b c";
    printf("main: %s\n", strtok(a, sp));
    srand(5);
    int first = rand();
    time_t t = 0;
    struct tm *tm = gmtime(&t);
    HANDLE h = CreateThread(NULL, 0, other, tm, 0, NULL);
    WaitForSingleObject(h, INFINITE);
    CloseHandle(h);
    char *b = strtok(NULL, sp);
    char *c = strtok(NULL, sp);
    printf("main: then %s %s\n", b ? b : "(null)", c ? c : "(null)");
    printf("main: gmtime mday still %d\n", tm->tm_mday);
    srand(5);
    printf("main: rand again equal %d\n", rand() == first);
    return 0;
}
