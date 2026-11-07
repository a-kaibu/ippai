// g++ (MinGW) C++ exceptions: on x64 libgcc's SEH unwinder runs each cleanup by raising
// STATUS_GCC_UNWIND from inside the unwind handler (RtlUnwindEx), for the frame being unwound
// to catch, and resumes with _Unwind_Resume; on x86 it is DWARF. Same output on both.
#include <cstdio>
#include <exception>
#include <stdexcept>
#include <string>
#include <vector>

static std::string log_;

struct Guard {
    int id;
    explicit Guard(int i) : id(i) {}
    ~Guard() { log_ += "~" + std::to_string(id) + " "; }
};

struct Base : std::exception {
    int v;
    explicit Base(int x) : v(x) {}
    const char *what() const noexcept override { return "base"; }
};
struct Derived : Base {
    explicit Derived(int x) : Base(x) {}
    const char *what() const noexcept override { return "derived"; }
};

__attribute__((noinline)) static int deep(int n, int at) {
    Guard g(n);
    if (n == at) throw Derived(n * 10);
    return deep(n + 1, at) + 1;
}

__attribute__((noinline)) static void rethrower(int n) {
    Guard g(100 + n);
    try {
        deep(0, n);
    } catch (Base &) {
        log_ += "inner ";
        throw;
    }
}

__attribute__((noinline)) static void catch_all_cleanup(int n) {
    std::vector<int> v(n, 7);
    try {
        if (n > 2) throw n;
    } catch (...) {
        log_ += "all(" + std::to_string(v.size()) + ") ";
        throw;
    }
}

__attribute__((noinline)) static std::string strings(int n) {
    std::string s(50, 'x');
    if (n & 1) throw std::runtime_error("odd " + std::to_string(n) + " " + s.substr(0, 3));
    return s;
}

int main() {
    // through cleanups to a catch by base class
    for (int at = 0; at < 4; at++) {
        log_.clear();
        try {
            deep(0, at);
        } catch (Base &e) {
            std::printf("deep %d: %s %d [%s]\n", at, e.what(), e.v, log_.c_str());
        }
    }
    // rethrow from a catch block, through more cleanups
    log_.clear();
    try {
        rethrower(2);
    } catch (Derived &e) {
        std::printf("rethrow: %d [%s]\n", e.v, log_.c_str());
    }
    // catch (...) { throw; } with a vector to free
    for (int n = 1; n < 5; n++) {
        log_.clear();
        try {
            catch_all_cleanup(n);
            std::printf("none %d\n", n);
        } catch (int x) {
            std::printf("int %d [%s]\n", x, log_.c_str());
        }
    }
    // std::runtime_error with allocated strings
    int ok = 0, bad = 0;
    for (int n = 0; n < 200; n++) {
        try {
            ok += (int)strings(n).size() / 50;
        } catch (std::exception &e) {
            bad += e.what()[0] == 'o';
        }
    }
    std::printf("strings: %d %d\n", ok, bad);
    // a throw inside a catch block, caught there; then exception_ptr
    try {
        try {
            throw 1;
        } catch (int) {
            try {
                throw std::string("nested");
            } catch (std::string &s) {
                std::printf("nested: %s\n", s.c_str());
            }
            throw 2.5;
        }
    } catch (double d) {
        std::printf("double: %.1f\n", d);
    }
    std::exception_ptr p;
    try {
        deep(0, 1);
    } catch (...) {
        p = std::current_exception();
    }
    try {
        std::rethrow_exception(p);
    } catch (Derived &e) {
        std::printf("exception_ptr: %d\n", e.v);
    }
    // many throws (the unwinder's state must not pile up)
    long sum = 0;
    for (int i = 0; i < 3000; i++) {
        try {
            deep(0, i % 5);
        } catch (Base &e) {
            sum += e.v;
        }
    }
    std::printf("loop: %ld\n", sum);
    return 0;
}
