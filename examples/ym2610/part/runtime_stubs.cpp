/* The pieces of Verilator's runtime that a part cannot have.
 *
 * Verilator's runtime assumes a hosted POSIX process: it builds a thread pool
 * out of pthreads, prints through stdio, calls exit() on $fatal and reads the
 * environment. A part is a wasm component with none of those, so the build
 * defines VL_USER_FATAL and VL_PRINTF and links the definitions below instead.
 */
#include "verilated.h"
#include "verilated_threads.h"

#include <cstdarg>
#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <ctime>
#include <pthread.h>
#include <sys/uio.h>

/* VerilatedContext::threadPoolp() references the pool even when the model is
 * single-threaded, but only constructs it when threads != 1, which never
 * happens here. */
VlThreadPool::VlThreadPool(VerilatedContext* contextp, unsigned nThreads) {
    (void)contextp;
    (void)nThreads;
}

VlThreadPool::~VlThreadPool() = default;

/* $fatal has nowhere to report to; trap so the host sees a failed call rather
 * than a part that silently keeps running. This is also the definition libc
 * and libc++ get, so a failing allocation lands here instead of in wasi-libc's
 * abort, which would import proc_exit. */
extern "C" void abort(void) {
    __builtin_trap();
}

void vl_fatal(const char* filename, int linenum, const char* hier, const char* msg) {
    (void)filename;
    (void)linenum;
    (void)hier;
    (void)msg;
    abort();
}

/* VerilatedContext::VerilatedContext() reads VERILATOR_SOLVER and friends.
 * A component has no environment, so every variable reads as unset. */
extern "C" char* getenv(const char* name) {
    (void)name;
    return nullptr;
}

/* The context destructor restores stdout/stderr redirection even when none was
 * set up, and the model records construction time. Nothing of that reaches a
 * part through the host ABI; the stubs keep wasi-libc's file and clock syscall
 * wrappers, and with them WASI imports, out of the link. */
extern "C" int close(int fd) {
    (void)fd;
    return -1;
}

extern "C" int dup(int fd) {
    (void)fd;
    return -1;
}

extern "C" int dup2(int from, int to) {
    (void)from;
    (void)to;
    return -1;
}

extern "C" int fflush(FILE* stream) {
    (void)stream;
    return 0;
}

extern "C" int clock_gettime(clockid_t clock, struct timespec* time) {
    (void)clock;
    if (time) {
        time->tv_sec = 0;
        time->tv_nsec = 0;
    }
    return 0;
}

/* Formatting a warning string is the tail of the report path, and libc's
 * printf pulls in the whole stdio writer, which ends in WASI's fd_write. The
 * formatted text is only ever handed to VL_PRINTF, which drops it, so an
 * empty string is enough. */
extern "C" int vsnprintf(char* buffer, size_t size, const char* format, va_list args) {
    (void)format;
    (void)args;
    if (size) {
        buffer[0] = '\0';
    }
    return 0;
}

/* libc++abi reports through this before calling abort(); the members it lives
 * with (the cxa_* handlers) are linked anyway, and its own definition drags in
 * printf. The part has no output channel, so trap instead. */
extern "C" void __abort_message(const char* format, ...) {
    (void)format;
    __builtin_trap();
}

/* Verilator parses +verilator+ command-line switches out of an argv a part
 * never has. libc's scanf half shares the printf half's output machinery, and
 * that is what still reaches stdout here, so the entry points stop at the
 * stub. */
extern "C" int sscanf(const char* input, const char* format, ...) {
    (void)input;
    (void)format;
    return EOF;
}

extern "C" int vfscanf(FILE* stream, const char* format, va_list args) {
    (void)stream;
    (void)format;
    (void)args;
    return EOF;
}

/* The FILE objects for stdin/stdout/stderr survive as data even though nothing
 * writes to them any more, and their vtables name these syscall shims. Kept
 * local, the shims stop wasi-libc's __wasi_* wrappers, and with them the WASI
 * imports, from entering the module. None of them is called. */
extern "C" ssize_t writev(int fd, const struct iovec* iovs, int count) {
    (void)fd;
    (void)iovs;
    (void)count;
    return 0;
}

extern "C" off_t __lseek(int fd, off_t offset, int whence) {
    (void)fd;
    (void)offset;
    (void)whence;
    return -1;
}

extern "C" int __isatty(int fd) {
    (void)fd;
    return 0;
}

extern "C" int __clock_gettime(clockid_t clock, struct timespec* time) {
    (void)clock;
    if (time) {
        time->tv_sec = 0;
        time->tv_nsec = 0;
    }
    return 0;
}

/* libc++'s stream number parsing calls strtold_l, which calls strtod, and
 * libc's strtod reports that wasm has no long double by writing to stderr --
 * which pulls all of stdio in. Nothing in a part streams a number in, so the
 * conversion family stops here. (They share strtod.c.obj, so all three are
 * defined to keep that object out of the link.) */
extern "C" double strtod(const char* text, char** end) {
    (void)text;
    if (end) {
        *end = const_cast<char*>(text);
    }
    return 0;
}

extern "C" float strtof(const char* text, char** end) {
    (void)text;
    if (end) {
        *end = const_cast<char*>(text);
    }
    return 0;
}

extern "C" long double strtold(const char* text, char** end) {
    (void)text;
    if (end) {
        *end = const_cast<char*>(text);
    }
    return 0;
}

/* A part runs on one thread, so the runtime's locks and queues never contend.
 * Locally defined, they keep wasi-libc's pthread implementation out of the
 * module; that implementation waits on futexes, which read the clock. */
extern "C" int pthread_mutex_init(pthread_mutex_t* mutex, const pthread_mutexattr_t* attr) {
    (void)mutex;
    (void)attr;
    return 0;
}

extern "C" int pthread_mutex_lock(pthread_mutex_t* mutex) {
    (void)mutex;
    return 0;
}

extern "C" int pthread_mutex_trylock(pthread_mutex_t* mutex) {
    (void)mutex;
    return 0;
}

extern "C" int pthread_mutex_unlock(pthread_mutex_t* mutex) {
    (void)mutex;
    return 0;
}

extern "C" int pthread_mutex_destroy(pthread_mutex_t* mutex) {
    (void)mutex;
    return 0;
}

extern "C" int pthread_cond_init(pthread_cond_t* cond, const pthread_condattr_t* attr) {
    (void)cond;
    (void)attr;
    return 0;
}

extern "C" int pthread_cond_wait(pthread_cond_t* cond, pthread_mutex_t* mutex) {
    (void)cond;
    (void)mutex;
    return 0;
}

extern "C" int pthread_cond_timedwait(pthread_cond_t* cond, pthread_mutex_t* mutex,
                                      const struct timespec* until) {
    (void)cond;
    (void)mutex;
    (void)until;
    return 0;
}

extern "C" int pthread_cond_signal(pthread_cond_t* cond) {
    (void)cond;
    return 0;
}

extern "C" int pthread_cond_broadcast(pthread_cond_t* cond) {
    (void)cond;
    return 0;
}

extern "C" int pthread_cond_destroy(pthread_cond_t* cond) {
    (void)cond;
    return 0;
}

/* Same for libc++'s assertion-reporting hook, which prints to stderr. */
extern "C" void __libcpp_verbose_abort(const char* format, ...)
    __asm__("_ZNSt3__222__libcpp_verbose_abortEPKcz");
extern "C" void __libcpp_verbose_abort(const char* format, ...) {
    (void)format;
    __builtin_trap();
}
