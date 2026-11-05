/* C++ the way engines are written: the standard library and virtual calls. Each case reports ms.
 *
 *   map        std::map<std::string, int> inserts and lookups (script variables, resource tables)
 *   string     std::string building and searching (text processing, tag parsing)
 *   vector     std::vector of structs: push_back, std::sort, erase (sprite lists sorted by depth)
 *   virtual    virtual calls through a scene graph of objects (update/draw)
 *   exceptions throwing and catching (some engines use them for control flow; the unwinder)
 *   unordered  std::unordered_map<int, int> (handles, caches)
 *
 * Build: i686-w64-mingw32-g++ -O2 -static cxx.cpp -o cxx.exe */
#include "bench.h"
#include <algorithm>
#include <map>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <vector>

static void case_map()
{
    unsigned n = (unsigned)(60000 * bench_scale) + 1, check = 0;
    double t0 = now_ms();
    std::map<std::string, int> m;
    char key[32];
    for (unsigned i = 0; i < n; i++)
    {
        snprintf(key, sizeof key, "f.var%u", i * 7919u % 50000u);
        m[key] += (int)i;
    }
    for (unsigned i = 0; i < n * 2; i++)
    {
        snprintf(key, sizeof key, "f.var%u", i % 60000u);
        auto it = m.find(key);
        if (it != m.end()) check += (unsigned)it->second;
    }
    bench_report("map", now_ms() - t0);
    bench_check("map", check + (unsigned)m.size());
}

static void case_string()
{
    unsigned n = (unsigned)(30000 * bench_scale) + 1, check = 0;
    double t0 = now_ms();
    for (unsigned i = 0; i < n; i++)
    {
        std::string s = "[chara name=\"sakura\" face=smile pos=";
        s += std::to_string(i % 3);
        s += "]";
        s += "Hello world, this is line ";
        s += std::to_string(i);
        s += "[l][r]";
        size_t at = s.find("face=");
        std::string face = s.substr(at + 5, s.find(' ', at) - at - 5);
        size_t tags = 0;
        for (size_t p = s.find('['); p != std::string::npos; p = s.find('[', p + 1)) tags++;
        check += (unsigned)(face.size() + tags + s.size());
    }
    bench_report("string", now_ms() - t0);
    bench_check("string", check);
}

struct Sprite
{
    int z, x, y, id;
    float alpha;
};

static void case_vector()
{
    unsigned reps = (unsigned)(40 * bench_scale) + 1, check = 0;
    double t0 = now_ms();
    for (unsigned r = 0; r < reps; r++)
    {
        std::vector<Sprite> v;
        for (int i = 0; i < 20000; i++) v.push_back(Sprite{ (int)(rnd() % 1000), i, -i, i, 0.5f });
        std::sort(v.begin(), v.end(), [](const Sprite &a, const Sprite &b) { return a.z != b.z ? a.z < b.z : a.id < b.id; });
        v.erase(std::remove_if(v.begin(), v.end(), [](const Sprite &s) { return s.z % 5 == 0; }), v.end());
        check += (unsigned)v.size() + (unsigned)v[v.size() / 2].id;
    }
    bench_report("vector", now_ms() - t0);
    bench_check("vector", check);
}

struct Node
{
    virtual ~Node() {}
    virtual int update(int t) = 0;
    virtual int draw(int acc) const = 0;
};
struct Layer : Node
{
    int v = 1;
    int update(int t) override { return v = (v * 3 + t) & 1023; }
    int draw(int acc) const override { return acc + v; }
};
struct Text : Node
{
    int chars = 0;
    int update(int t) override { return chars = (chars + 1) % (t % 40 + 1); }
    int draw(int acc) const override { return acc ^ chars; }
};
struct Fade : Node
{
    float a = 0;
    int update(int t) override { a = (t % 60) / 60.0f; return (int)(a * 255); }
    int draw(int acc) const override { return acc + (int)(a * 3); }
};

static void case_virtual()
{
    std::vector<Node *> nodes;
    for (int i = 0; i < 300; i++)
        nodes.push_back(i % 3 == 0 ? (Node *)new Layer : i % 3 == 1 ? (Node *)new Text : (Node *)new Fade);
    unsigned frames = (unsigned)(6000 * bench_scale) + 1;
    int acc = 0;
    double t0 = now_ms();
    for (unsigned f = 0; f < frames; f++)
    {
        for (Node *n : nodes) acc += n->update((int)f);
        for (Node *n : nodes) acc = n->draw(acc);
    }
    bench_report("virtual", now_ms() - t0);
    bench_check("virtual", (unsigned)acc);
    for (Node *n : nodes) delete n;
}

static int deep(int d, int i)
{
    if (!d)
    {
        if (i % 3 == 0) throw std::runtime_error("skip");
        return i;
    }
    std::string guard("x"); /* (a destructor to run while unwinding) */
    return deep(d - 1, i) + (int)guard.size();
}

static void case_exceptions()
{
    unsigned n = (unsigned)(20000 * bench_scale) + 1, check = 0;
    double t0 = now_ms();
    for (unsigned i = 0; i < n; i++)
    {
        try
        {
            check += (unsigned)deep(4, (int)i);
        }
        catch (const std::exception &e)
        {
            check += (unsigned)e.what()[0];
        }
    }
    bench_report("exceptions", now_ms() - t0);
    bench_check("exceptions", check);
}

static void case_unordered()
{
    unsigned n = (unsigned)(300000 * bench_scale) + 1, check = 0;
    double t0 = now_ms();
    std::unordered_map<int, int> m;
    for (unsigned i = 0; i < n; i++) m[(int)(i * 2654435761u % 100000u)] += (int)i;
    for (unsigned i = 0; i < n; i++)
    {
        auto it = m.find((int)i % 120000);
        if (it != m.end()) check += (unsigned)it->second;
    }
    bench_report("unordered", now_ms() - t0);
    bench_check("unordered", check + (unsigned)m.size());
}

int main()
{
    bench_args();
    if (bench_is("map")) case_map();
    if (bench_is("string")) case_string();
    if (bench_is("vector")) case_vector();
    if (bench_is("virtual")) case_virtual();
    if (bench_is("exceptions")) case_exceptions();
    if (bench_is("unordered")) case_unordered();
    bench_end();
    return 0;
}
