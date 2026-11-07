// msstl
// iostreams of msvcp140 (x64, built against Microsoft's STL headers): number formatting
// (flags, width, fill, precision, bases), parsing (bases, failures, ranges, eof), unformatted
// input, seeking, string and file streams, wide streams, stream state and exceptions, iword /
// pword / callbacks, the locale facets (codecvt, ctype<wchar_t>, time_put, collate), wregex,
// and cout / cerr / wcout sharing stdout with printf. Every line is compared with Windows' own output (tests/winref).
#include <cstdio>
#include <ctime>
#include <cwchar>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <locale>
#include <regex>
#include <sstream>
#include <string>

extern "C" __declspec(dllimport) void __stdcall ExitProcess(unsigned);

static void show(const char* what, const std::string& s) {
    std::printf("%s [%s]\n", what, s.c_str());
}
static void wshow(const char* what, const std::wstring& s) {
    std::string n;
    for (wchar_t c : s) n += c < 128 ? static_cast<char>(c) : '?';
    std::printf("%s [%s]\n", what, n.c_str());
}
static int st(const std::ios_base& s) {
    return s.rdstate();
}

static void out() {
    std::ostringstream o;
    o << 42 << ' ' << -7 << ' ' << 0 << ' ' << 123456789012345LL << ' ' << 18446744073709551615ULL;
    show("ints", o.str());
    o.str("");
    o << std::hex << 255 << ' ' << -1 << ' ' << std::showbase << 255 << ' ' << std::uppercase << 255 << ' '
      << std::oct << 8 << ' ' << std::noshowbase << 8 << std::dec << std::nouppercase;
    show("bases", o.str());
    o.str("");
    o << std::showpos << 5 << ' ' << 0 << ' ' << -5 << std::noshowpos;
    show("showpos", o.str());
    o.str("");
    o << '[' << std::setw(6) << 42 << ']' << '[' << std::left << std::setw(6) << 42 << ']' << std::right << '['
      << std::internal << std::setw(6) << -42 << ']' << '[' << std::setfill('0') << std::setw(8) << std::hex
      << std::showbase << 255 << ']' << std::dec << std::noshowbase << std::setfill(' ') << std::right;
    show("width", o.str());
    o.str("");
    o << 1.5 << ' ' << 0.1 << ' ' << 1e21 << ' ' << 1234567.0 << ' ' << 3.14159265358979 << ' ' << -0.0 << ' '
      << 100.0;
    show("double", o.str());
    o.str("");
    o << std::fixed << 1.5 << ' ' << std::setprecision(2) << 3.14159 << ' ' << std::setprecision(0) << 2.5 << ' '
      << std::scientific << std::setprecision(3) << 12345.678 << ' ' << std::uppercase << 0.000123
      << std::nouppercase << std::defaultfloat << std::setprecision(6);
    show("fixed", o.str());
    o.str("");
    o << std::showpoint << 1.0 << ' ' << 100.0 << std::noshowpoint << ' ' << std::setprecision(10) << 1.0 / 3
      << std::setprecision(6) << ' ' << 1.25f << ' ' << std::hexfloat << 1.0 << ' ' << 0.5 << std::defaultfloat;
    show("point", o.str());
    o.str("");
    o << true << ' ' << false << ' ' << std::boolalpha << true << ' ' << false << ' ' << std::setw(7) << true
      << std::noboolalpha;
    show("bool", o.str());
    o.str("");
    o << static_cast<short>(-3) << ' ' << std::hex << static_cast<short>(-3) << std::dec << ' '
      << static_cast<unsigned short>(65535) << ' ' << 4000000000u << ' ' << -2147483647L - 1 << ' '
      << 4294967295UL;
    show("widths", o.str());
    o.str("");
    o << "text" << ' ' << std::setw(6) << "ab" << '|' << std::left << std::setw(4) << 'x' << '|' << std::right;
    o.put('!').write("xyz", 2);
    o << std::string("str");
    show("chars", o.str());
    std::printf("tellp %lld\n", static_cast<long long>(o.tellp()));
    o.seekp(2);
    o << "##";
    show("seekp", o.str());
}

static void in() {
    {
        std::istringstream i("  42 -17 +8 0x1f 017 2147483648 -2147483649 abc");
        int a = 0, b = 0, c = 0, d = 0, e = 0, f = 0, g = 0, h = 9;
        i >> a >> b >> c;
        std::printf("ints %d %d %d state %d\n", a, b, c, st(i));
        i >> std::hex >> d >> std::oct >> e >> std::dec;
        std::printf("bases %d %d state %d\n", d, e, st(i));
        i >> f;
        std::printf("too big %d state %d\n", f, st(i));
        i.clear();
        i >> g;
        std::printf("too small %d state %d\n", g, st(i));
        i.clear();
        i >> h;
        std::printf("not a number %d state %d\n", h, st(i));
    }
    {
        std::istringstream i("0x10 010 10");
        int a, b, c;
        i.unsetf(std::ios::basefield);
        i >> a >> b >> c;
        std::printf("auto base %d %d %d state %d\n", a, b, c, st(i));
    }
    {
        std::istringstream i("3.25 -1e3 .5 1e400 7");
        double a, b, c, d;
        float e;
        i >> a >> b >> c;
        std::printf("doubles %g %g %g state %d\n", a, b, c, st(i));
        i >> d;
        std::printf("huge state %d\n", st(i));
        i.clear();
        i >> e;
        std::printf("float %g state %d\n", e, st(i));
    }
    {
        std::istringstream i("1 0 2 true false x");
        bool a, b, c, d, e;
        i >> a >> b;
        std::printf("bools %d %d state %d\n", a, b, st(i));
        i >> c;
        std::printf("bool 2 %d state %d\n", c, st(i));
        i.clear();
        i >> std::boolalpha >> d >> e;
        std::printf("alpha %d %d state %d\n", d, e, st(i));
    }
    {
        std::istringstream i("40000 -5 65535 18446744073709551615 -9223372036854775808 99999999999");
        short s;
        unsigned short us;
        unsigned u;
        unsigned long long ull;
        long long ll;
        long l;
        i >> s;
        std::printf("short %d state %d\n", s, st(i));
        i.clear();
        i >> us;
        std::printf("ushort -5 %u state %d\n", us, st(i));
        i >> us >> ull >> ll;
        std::printf("more %u %llu %lld state %d\n", us, ull, ll, st(i));
        i >> l;
        std::printf("long %ld state %d\n", l, st(i));
        (void) u;
    }
    {
        std::istringstream i("line one\nline two\n\nlast");
        char buf[64];
        i.getline(buf, sizeof buf);
        std::printf("getline [%s] %lld\n", buf, static_cast<long long>(i.gcount()));
        std::string s;
        std::getline(i, s);
        show("std::getline", s);
        i.getline(buf, sizeof buf);
        std::printf("empty [%s] %lld state %d\n", buf, static_cast<long long>(i.gcount()), st(i));
        i.getline(buf, 3);
        std::printf("short buffer [%s] %lld state %d\n", buf, static_cast<long long>(i.gcount()), st(i));
        i.clear();
        i.getline(buf, sizeof buf);
        std::printf("at end [%s] state %d\n", buf, st(i));
    }
    {
        std::istringstream i("abcdef");
        char ch = 0;
        int p = i.peek();
        i.get(ch);
        int g = i.get();
        std::printf("peek %c get %c get %c\n", p, ch, g);
        i.putback('b');
        i.unget();
        std::printf("after putback/unget peek %c state %d\n", i.peek(), st(i));
        i.ignore(2);
        char buf[8] = {};
        i.read(buf, 3);
        std::printf("read [%s] %lld state %d\n", buf, static_cast<long long>(i.gcount()), st(i));
        i.read(buf, 5);
        std::printf("short read %lld state %d\n", static_cast<long long>(i.gcount()), st(i));
        i.clear();
        i.seekg(1);
        std::printf("tellg %lld\n", static_cast<long long>(i.tellg()));
        i.seekg(-2, std::ios::end);
        std::printf("from end %c\n", i.get());
        i.ignore(100, 'x');
        std::printf("ignore to end state %d\n", st(i));
        i.clear();
        i.seekg(0);
        char b2[8] = {};
        i.get(b2, 4, 'd');
        std::printf("get delim [%s] next %c\n", b2, i.peek());
        std::printf("readsome %lld\n", static_cast<long long>(i.readsome(b2, 2)));
    }
    {
        std::istringstream i("word1   word2\tword3");
        std::string a, b, c, d;
        i >> a >> b >> c >> d;
        std::printf("words [%s] [%s] [%s] [%s] state %d\n", a.c_str(), b.c_str(), c.c_str(), d.c_str(), st(i));
    }
}

static void files() {
    const char* name = "msvcstream.tmp";
    {
        std::ofstream f(name, std::ios::binary);
        f << "first " << 1 << '\n' << "second " << 2.5 << '\n';
        f.write("tail", 4);
        std::printf("ofstream open %d good %d\n", f.is_open(), f.good());
    }
    {
        std::ifstream f(name, std::ios::binary);
        std::string w;
        int n;
        double d;
        f >> w >> n;
        show("ifstream word", w);
        std::printf("ifstream int %d\n", n);
        f >> w >> d;
        std::printf("ifstream %s %g\n", w.c_str(), d);
        f.seekg(0, std::ios::end);
        std::printf("size %lld\n", static_cast<long long>(f.tellg()));
        f.seekg(-4, std::ios::end);
        char t[5] = {};
        f.read(t, 4);
        std::printf("tail [%s] state %d\n", t, st(f));
        f.read(t, 1);
        std::printf("past end state %d\n", st(f));
    }
    {
        std::fstream f(name, std::ios::in | std::ios::out | std::ios::binary);
        f.seekp(0);
        f << "FIRST";
        f.seekg(0);
        std::string w;
        f >> w;
        show("fstream rewrite", w);
    }
    {
        std::ofstream f(name, std::ios::app);
        f << "\nappended";
    }
    {
        std::ifstream f(name);
        std::string l, last;
        int lines = 0;
        while (std::getline(f, l)) {
            last = l;
            ++lines;
        }
        std::printf("lines %d last [%s]\n", lines, last.c_str());
    }
    {
        std::ifstream f("no-such-file.txt");
        std::printf("missing open %d state %d\n", f.is_open(), st(f));
    }
    std::remove(name);
}

static void wide() {
    std::wostringstream o;
    o << L"wide " << 42 << L' ' << 2.5 << L' ' << std::hex << 255 << std::dec << L' ' << std::setw(5)
      << std::setfill(L'.') << 7;
    wshow("wostringstream", o.str());
    std::wistringstream i(L"  12 3.5 word");
    int n;
    double d;
    std::wstring w;
    i >> n >> d >> w;
    std::printf("wistringstream %d %g state %d\n", n, d, st(i));
    wshow("wide word", w);
    wchar_t buf[8];
    std::wistringstream j(L"ab\ncd");
    j.getline(buf, 8);
    wshow("wide getline", buf);
}

static void facets() {
    std::locale loc;
    using Cvt = std::codecvt<char, char, std::mbstate_t>;
    const Cvt& cc = std::use_facet<Cvt>(loc);
    std::mbstate_t ms{};
    const char src[] = "abc";
    const char* fn = nullptr;
    char dst[8];
    char* tn = nullptr;
    int r = cc.in(ms, src, src + 3, fn, dst, dst + 8, tn);
    std::printf("codecvt<char> noconv %d in %d next %d %d max %d enc %d len %d\n", cc.always_noconv(), r,
        (int) (fn - src), (int) (tn - dst), cc.max_length(), cc.encoding(), cc.length(ms, src, src + 3, 2));
    r = cc.out(ms, src, src + 3, fn, dst, dst + 8, tn);
    std::printf("codecvt<char> out %d unshift %d\n", r, cc.unshift(ms, dst, dst + 8, tn));

    using WCvt = std::codecvt<wchar_t, char, std::mbstate_t>;
    const WCvt& wc = std::use_facet<WCvt>(loc);
    const char bytes[] = "h\xe9llo";
    wchar_t wide[8] = {};
    wchar_t* wn = nullptr;
    r = wc.in(ms, bytes, bytes + 5, fn, wide, wide + 8, wn);
    std::printf("codecvt<wchar_t> noconv %d in %d %04x %d %d max %d enc %d\n", wc.always_noconv(), r, wide[1],
        (int) (fn - bytes), (int) (wn - wide), wc.max_length(), wc.encoding());
    r = wc.in(ms, bytes, bytes + 5, fn, wide, wide + 2, wn);
    std::printf("in partial %d %d %d\n", r, (int) (fn - bytes), (int) (wn - wide));
    const wchar_t ws[] = L"ok\x3042!";
    const wchar_t* wfn = nullptr;
    r = wc.out(ms, ws, ws + 4, wfn, dst, dst + 8, tn);
    std::printf("out error %d at %d %d\n", r, (int) (wfn - ws), (int) (tn - dst));
    r = wc.out(ms, ws, ws + 2, wfn, dst, dst + 1, tn);
    std::printf("out partial %d %d %d\n", r, (int) (wfn - ws), (int) (tn - dst));
    r = wc.unshift(ms, dst, dst + 8, tn);
    std::printf("unshift %d %d length %d\n", r, (int) (tn - dst), wc.length(ms, bytes, bytes + 5, 3));

    const std::ctype<wchar_t>& ct = std::use_facet<std::ctype<wchar_t>>(loc);
    std::printf("ctype<wchar_t> alpha %d %d %d digit %d %d space %d %d upper %d punct %d %d\n",
        ct.is(std::ctype_base::alpha, L'a'), ct.is(std::ctype_base::alpha, L'\x3042'), ct.is(std::ctype_base::alpha, L'1'),
        ct.is(std::ctype_base::digit, L'7'), ct.is(std::ctype_base::digit, L'x'), ct.is(std::ctype_base::space, L' '),
        ct.is(std::ctype_base::space, L'\x3000'), ct.is(std::ctype_base::upper, L'Q'), ct.is(std::ctype_base::punct, L'!'),
        ct.is(std::ctype_base::punct, L'\x3001'));
    std::printf("tolower %04x %04x %04x toupper %04x %04x\n", ct.tolower(L'A'), ct.tolower(L'\xc0'), ct.tolower(L'\x391'),
        ct.toupper(L'b'), ct.toupper(L'\x3b1'));
    wchar_t buf[] = L"MiXeD 1";
    ct.tolower(buf, buf + 7);
    wshow("tolower range", buf);
    ct.toupper(buf, buf + 7);
    wshow("toupper range", buf);
    std::printf("widen %04x %04x narrow %c %c\n", ct.widen('z'), ct.widen('\xe9'), ct.narrow(L'q', '?'), ct.narrow(L'\x3042', '?'));
    char nb[4];
    ct.narrow(L"a\x100z", L"a\x100z" + 3, '#', nb);
    nb[3] = 0;
    std::wstring wb(3, L' ');
    ct.widen("xyz", "xyz" + 3, &wb[0]);
    std::printf("narrow range [%s] ", nb);
    wshow("widen range", wb);
    const wchar_t mix[] = L"ab 12";
    std::printf("scan_is digit %d scan_not alpha %d\n", (int) (ct.scan_is(std::ctype_base::digit, mix, mix + 5) - mix),
        (int) (ct.scan_not(std::ctype_base::alpha, mix, mix + 5) - mix));
    std::ctype_base::mask m[3];
    ct.is(L"A1 ", L"A1 " + 3, m);
    std::printf("masks %d %d %d\n", (m[0] & std::ctype_base::upper) != 0, (m[1] & std::ctype_base::digit) != 0,
        (m[2] & std::ctype_base::space) != 0);

    const std::collate<wchar_t>& co = std::use_facet<std::collate<wchar_t>>(loc);
    const wchar_t a[] = L"apple", b[] = L"apply", c2[] = L"app";
    std::printf("collate %d %d %d %d\n", co.compare(a, a + 5, b, b + 5), co.compare(b, b + 5, a, a + 5),
        co.compare(c2, c2 + 3, a, a + 5), co.compare(a, a + 5, a, a + 5));
    wshow("transform", co.transform(a, a + 5));
    const std::collate<char>& cn = std::use_facet<std::collate<char>>(loc);
    std::printf("collate<char> %d %d\n", cn.compare("b", "b" + 1, "a", "a" + 1), cn.compare("a", "a" + 1, "ab", "ab" + 2));

    std::tm t{};
    t.tm_year = 124;
    t.tm_mon = 1;
    t.tm_mday = 29;
    t.tm_hour = 13;
    t.tm_min = 5;
    t.tm_sec = 9;
    t.tm_wday = 4;
    t.tm_yday = 59;
    std::wostringstream wo;
    wo << std::put_time(&t, L"%Y-%m-%d %H:%M:%S %a %b %j %% %I%p");
    wshow("put_time", wo.str());
    std::wostringstream wo2;
    const std::time_put<wchar_t>& tp = std::use_facet<std::time_put<wchar_t>>(loc);
    tp.put(std::ostreambuf_iterator<wchar_t>(wo2), wo2, L' ', &t, 'y');
    tp.put(std::ostreambuf_iterator<wchar_t>(wo2), wo2, L' ', &t, 'd', '#');
    const wchar_t pat[] = L"<%H|%#m|%%|%Ey>";
    tp.put(std::ostreambuf_iterator<wchar_t>(wo2), wo2, L' ', &t, pat, pat + 15);
    wshow("time_put", wo2.str());
    std::ostringstream no;
    no << std::put_time(&t, "%c|%x|%X");
    show("put_time char", no.str());

    std::wregex re(L"([a-c]+)(\\d*)", std::regex::icase);
    std::wsmatch mt;
    std::wstring subject = L"xxABcab42yy";
    bool found = std::regex_search(subject, mt, re);
    std::printf("wregex %d ", found);
    wshow("match", mt.str(0));
    std::wregex rc(L"[[:alpha:]]+[[:digit:]]", std::regex::collate);
    std::printf("wregex classes %d %d\n", std::regex_match(std::wstring(L"abc1"), rc), std::regex_match(std::wstring(L"ab-1"), rc));
    std::regex nr("^(\\w+)@(\\w+)\\.com$");
    std::smatch sm;
    std::string mail = "user@host.com";
    std::printf("regex %d ", std::regex_match(mail, sm, nr));
    show("groups", sm.str(1) + "," + sm.str(2));

    std::wofstream wf("msvcstream.tmp");
    wf << L"wide file " << 7 << L'\n';
    std::printf("wofstream good %d ", wf.good());
    wf << L"\x3042";
    std::printf("after U+3042 state %d\n", st(wf));
    wf.close();
    std::wifstream wi("msvcstream.tmp");
    std::wstring line;
    std::getline(wi, line);
    wshow("wifstream", line);
    std::remove("msvcstream.tmp");
}

static int callbacks;
static void on_event(std::ios_base::event e, std::ios_base&, int idx) {
    callbacks += (e + 1) * 10 + idx;
}

static void state() {
    std::istringstream i("x");
    i.exceptions(std::ios::failbit);
    int n;
    try {
        i >> n;
        std::printf("no throw\n");
    } catch (const std::ios_base::failure& e) {
        std::printf("failure caught state %d\n", st(i));
    }
    std::stringstream s;
    try {
        s.exceptions(std::ios::badbit);
        s.clear(std::ios::badbit);
    } catch (const std::exception&) {
        std::printf("badbit caught\n");
    }
    std::ostringstream o;
    int idx = std::ios_base::xalloc();
    o.iword(idx) = 77;
    o.pword(idx) = &o;
    std::printf("iword %ld pword %d\n", o.iword(idx), o.pword(idx) == &o);
    o.register_callback(on_event, 3);
    std::ostringstream o2;
    o2.copyfmt(o);
    std::printf("copyfmt iword %ld callbacks %d\n", o2.iword(idx), callbacks);
    o.imbue(std::locale());
    std::printf("after imbue callbacks %d\n", callbacks);
    o << std::setw(4) << std::setfill('*') << std::hex;
    std::ostringstream o3;
    o3.copyfmt(o);
    o3 << 10;
    show("copied format", o3.str());
    std::printf("flags %x precision %lld width %lld\n", o.flags(), static_cast<long long>(o.precision()),
        static_cast<long long>(o.width()));
    std::ostream null(nullptr);
    std::printf("no buffer state %d\n", st(null));
    null.rdbuf(o.rdbuf());
    null << "via rdbuf";
    show("shared buffer", o.str());
    std::printf("bool ops %d %d\n", static_cast<bool>(o), !o);
}

int main() {
    out();
    in();
    files();
    wide();
    facets();
    state();
    std::cout << "cout " << 1 << ' ' << 2.5 << std::endl;
    std::wcout << L"wcout " << 3 << std::endl;
    std::printf("printf between\n");
    std::cout << "cout after printf" << std::endl;
    std::printf("cerr unitbuf %d tie %d cin tie %d\n", (std::cerr.flags() & std::ios_base::unitbuf) != 0,
        std::cerr.tie() == &std::cout, std::cin.tie() == &std::cout);
    // (cerr and clog write to stderr, which the comparison with Windows leaves out)
    std::cerr << "";
    std::fflush(stdout);
    // (built with /entry:main: no CRT startup to return to)
    ExitProcess(0);
}
