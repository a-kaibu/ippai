/* libc entry points vnhost wraps in C (the others: src/wrap.rs). */

#include <unistd.h>
#include <emscripten.h>

/* Emscripten's sched_yield() is a synchronous call to the browser's main thread, which then had
 * the round trips of every spin loop in Wine and the games to serve. Every thread has a worker
 * of its own: nothing to yield to. But a thread that does nothing else (a game polling
 * PeekMessage, which yields when the queue is empty, or spinning on Sleep(0)) would keep a core
 * busy for good: after YIELD_SPIN yields in a row less than a millisecond apart, each one sleeps
 * a millisecond (Softpal's main loop: a whole core before). */
#define YIELD_SPIN 64
int __wrap_sched_yield( void )
{
    static __thread double last;
    static __thread unsigned run;
    double now = emscripten_get_now();
    run = now - last < 1.0 ? run + 1 : 0;
    if (run >= YIELD_SPIN)
    {
        usleep( 1000 );
        now = emscripten_get_now();
        run = YIELD_SPIN;  /* (the sleep made the gap: keep sleeping while the spin goes on) */
        last = now - 0.5;
        return 0;
    }
    last = now;
    return 0;
}
