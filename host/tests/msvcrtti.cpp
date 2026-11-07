// vcruntime140's RTTI (x64, MSVC ABI, as cl's dynamic_cast / typeid compile to them):
// __RTDynamicCast down, up and across single, multiple and virtual inheritance, casts that must
// fail (a wrong type, a private base, an ambiguous base), dynamic_cast<void *> (__RTCastToVoid)
// and typeid of a polymorphic object (__RTtypeid). Prints only what any Windows gives. Built by
// build-msvc64.sh (clang, MSVC ABI, no SDK).
// (no SDK headers: std::type_info as vcruntime's, a TypeDescriptor: vftable, spare, the
// decorated name)
namespace std {
class type_info {
public:
    virtual ~type_info();
    bool operator==(const type_info &o) const {
        const char *a = name_, *b = o.name_;
        while (*a && *a == *b) a++, b++;
        return *a == *b;
    }
private:
    void *spare_;
    char name_[1];
};
}
extern "C" {
typedef void *HANDLE;
typedef unsigned long DWORD;
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) int __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);
}
// (type_info's vftable is no DLL's: msvcrt.lib has it; nothing here calls through it)
extern "C" const void *const type_info_vftable[2] asm("??_7type_info@@6B@") = {0, 0};

static void out(const char *s) {
    DWORD n = 0;
    while (s[n]) n++;
    WriteFile(GetStdHandle((DWORD)-11), s, n, &n, 0);
}
static void num(const char *label, long long v) {
    char b[96];
    int i = 0;
    while (*label) b[i++] = *label++;
    b[i++] = ' ';
    if (v < 0) { b[i++] = '-'; v = -v; }
    char t[24];
    int k = 0;
    do { t[k++] = '0' + v % 10; v /= 10; } while (v);
    while (k) b[i++] = t[--k];
    b[i++] = '\n';
    b[i] = 0;
    out(b);
}

// single inheritance: Animal <- Dog <- Puppy, Animal <- Cat
struct Animal { virtual void f() {} int legs = 4; };
struct Dog : Animal { int bark = 1; };
struct Puppy : Dog { int small = 2; };
struct Cat : Animal { int meow = 3; };

// multiple inheritance: Left, Right <- Both (Right not at offset 0); a private base
struct Left { virtual void f() {} int l = 10; };
struct Right { virtual void f() {} int r = 20; };
struct Hidden { virtual void f() {} int h = 30; };
struct Both : Left, Right, private Hidden { int b = 40; Hidden *hidden() { return this; } };

// virtual inheritance: Base <- (V1, V2 virtual) <- Diamond
struct Base { virtual void f() {} int base = 50; };
struct V1 : virtual Base { int v1 = 60; };
struct V2 : virtual Base { int v2 = 70; };
struct Diamond : V1, V2 { int d = 80; };

// an ambiguous base: Animal twice (through Dog and through Cat)
struct DogCat : Dog, Cat { int dc = 90; };

template <class T> static T *opaque(T *p) {
    asm volatile("" : "+r"(p));
    return p;
}

int main() {
    Puppy puppy;
    Animal *a = opaque<Animal>(&puppy);
    num("down to Dog", dynamic_cast<Dog *>(a) == static_cast<Dog *>(&puppy));
    num("down to Puppy", dynamic_cast<Puppy *>(a) == &puppy);
    num("Dog field", dynamic_cast<Dog *>(a)->bark);
    num("not a Cat", dynamic_cast<Cat *>(a) == nullptr);
    Cat cat;
    num("Cat is no Dog", dynamic_cast<Dog *>(opaque<Animal>(&cat)) == nullptr);
    num("null stays null", dynamic_cast<Dog *>(opaque<Animal>(nullptr)) == nullptr);

    Both both;
    Left *l = opaque<Left>(&both);
    Right *r = dynamic_cast<Right *>(l);
    num("cross to Right", r == static_cast<Right *>(&both));
    num("Right moved", (char *)r - (char *)l > 0);
    num("Right field", r->r);
    num("Right down to Both", dynamic_cast<Both *>(opaque<Right>(r)) == &both);
    num("Right back to Left", dynamic_cast<Left *>(opaque<Right>(r)) == l);
    num("void from Right", dynamic_cast<void *>(opaque<Right>(r)) == (void *)&both);
    num("private base from Left", dynamic_cast<Hidden *>(l) == nullptr);
    num("Both from private base", dynamic_cast<Both *>(opaque<Hidden>(both.hidden())) == nullptr);

    Diamond dia;
    Base *b = opaque<Base>(static_cast<Base *>(&dia));
    num("virtual base down to Diamond", dynamic_cast<Diamond *>(b) == &dia);
    num("virtual base to V2", dynamic_cast<V2 *>(b) == static_cast<V2 *>(&dia));
    num("V2 field", dynamic_cast<V2 *>(b)->v2);
    num("void from virtual base", dynamic_cast<void *>(b) == (void *)&dia);
    num("V1 to V2", dynamic_cast<V2 *>(opaque<V1>(&dia)) == static_cast<V2 *>(&dia));

    DogCat dc;
    Dog *dd = opaque<Dog>(&dc);
    num("ambiguous Animal from Dog", dynamic_cast<Animal *>(opaque<Cat>(&dc)) != nullptr);
    num("Dog to Cat across", dynamic_cast<Cat *>(dd) == static_cast<Cat *>(&dc));
    num("DogCat from Dog", dynamic_cast<DogCat *>(dd) == &dc);

    num("typeid Puppy", typeid(*a) == typeid(Puppy));
    num("typeid not Dog", typeid(*a) == typeid(Dog));
    Right *rr = opaque<Right>(r);
    num("typeid through Right", typeid(*rr) == typeid(Both));
    num("typeid through virtual base", typeid(*b) == typeid(Diamond));
    ExitProcess(0);
}
