/* libs: -lole32 */
/* d3dxof.dll (IDirectXFile): templates, data objects in text, binary (integer and float lists)
 * and MSZIP-compressed text, GetData's packing, nested children and references, GetName's
 * buffer rules, reference counts. Pygmalion 体験版 reads its motion.x files with it (it exited
 * with code 3 when DirectXFileCreate was missing). Loaded with LoadLibrary, as programs do. */
#define COBJMACROS
#include <windows.h>
#include <initguid.h>
#include <dxfile.h>
#include <stdio.h>
#include <string.h>

static const char templates[] =
    "xof 0302txt 0032\n"
    "template Vector { <3D82AB5E-62DA-11CF-AB39-0020AF71E433> FLOAT x; FLOAT y; FLOAT z; }\n"
    "template ColorRGBA { <35FF44E0-6C7C-11cf-8F52-0040333594A3> FLOAT red; FLOAT green; FLOAT blue; FLOAT alpha; }\n"
    "template ColorRGB { <D3E16E81-7835-11cf-8F52-0040333594A3> FLOAT red; FLOAT green; FLOAT blue; }\n"
    "template TextureFilename { <A42790E1-7810-11cf-8F52-0040333594A3> STRING filename; }\n"
    "template Material { <3D82AB4D-62DA-11CF-AB39-0020AF71E433> ColorRGBA faceColor; FLOAT power;"
    " ColorRGB specularColor; ColorRGB emissiveColor; [...] }\n"
    "template Matrix4x4 { <F6F23F45-7686-11cf-8F52-0040333594A3> array FLOAT matrix[16]; }\n"
    "template FrameTransformMatrix { <F6F23F41-7686-11cf-8F52-0040333594A3> Matrix4x4 frameMatrix; }\n"
    "template Frame { <3D82AB46-62DA-11CF-AB39-0020AF71E433> [...] }\n"
    "template Numbers { <11223344-5566-7788-99AA-BBCCDDEEFF00> DWORD n; array DWORD v[n]; STRING s; }\n"
    "template Words { <11223344-5566-7788-99AA-BBCCDDEEFF01> WORD a; CHAR b; DOUBLE c; array WORD w[2]; }\n";

static const char file_text[] =
    "xof 0302txt 0032\n"
    "// a comment\n"
    "template Local { <11223344-5566-7788-99AA-BBCCDDEEFF02> DWORD a; [Numbers] }\n"
    "Material Mat {\n 0.5;0.25;0.125;1.0;;\n 51.2;\n 0.0;0.0;0.0;;\n 1;2;3;;\n"
    " TextureFilename { \"tex\x82\xa0.psd\"; }\n}\n"
    "Frame Root { <01020304-0506-0708-090A-0B0C0D0E0F10>\n"
    " FrameTransformMatrix { 1.0,0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,0.0,1.0,0.0,10.0,20.0,30.0,1.0;; }\n"
    " Frame Child { Vector { 1;2;3; } { Mat } }\n"
    " { Later }\n"
    "}\n"
    "Local { 5; Numbers { 2; 4, 5; \"x;y\"; } }\n"
    "Words Later { 65535; 66; -0.5; 7, 8;; }\n";

/* the same Numbers and Vector as binary tokens: an integer list, then a float list */
static const unsigned char file_bin[] = {
    'x','o','f',' ','0','3','0','2','b','i','n',' ','0','0','3','2',
    1,0, 7,0,0,0, 'N','u','m','b','e','r','s', 1,0, 3,0,0,0, 'B','i','n', 10,0,
    6,0, 4,0,0,0, 3,0,0,0, 10,0,0,0, 20,0,0,0, 30,0,0,0,
    2,0, 2,0,0,0, 'o','k', 20,0,
    11,0,
    1,0, 6,0,0,0, 'V','e','c','t','o','r', 10,0,
    7,0, 3,0,0,0, 0,0,0xc0,0x3f, 0,0,0x10,0xc0, 0,0,0x40,0x40,
    11,0,
};

/* "Vector Packed { 1.5; -2.25; 3.0; }" and a Numbers, MSZIP-compressed (one block) */
static const char file_zip[] =
    "\x78\x6f\x66\x20\x30\x33\x30\x32\x74\x7a\x69\x70\x30\x30\x33\x32\x5a\x00\x00\x00\x4a\x00\x47\x00\x43\x4b\xe3\x0a\x4b\x4d\x2e\xc9"
    "\x2f\x52\x08\x48\x4c\xce\x4e\x4d\x51\xa8\x56\x30\xd4\x33\xb5\x56\xd0\x35\xd2\x33\x02\x52\xc6\x7a\x06\xd6\x0a\xb5\x5c\x7e\xa5\xb9"
    "\x49\xa9\x45\xc5\x0a\x51\x99\x05\x05\x60\x35\xc6\xd6\x0a\xe6\x3a\x0a\x16\x3a\x0a\x96\xd6\x0a\x4a\x55\x99\x05\x4a\x20\x55\x00";

static HRESULT (WINAPI *create)(IDirectXFile **);

static ULONG refs(void *p) {
    IUnknown *u = p;
    IUnknown_AddRef(u);
    return IUnknown_Release(u);
}

/* the data's bytes as numbers, strings followed through their pointers */
static void dump_data(const char *tname, BYTE *d, DWORD size) {
    DWORD i;
    if (!strcmp(tname, "TextureFilename") || !strcmp(tname, "Numbers")) {
        DWORD o = 0;
        if (!strcmp(tname, "Numbers")) {
            DWORD n = *(DWORD *)d;
            printf(" n=%lu", n);
            for (i = 0; i < n; i++) printf(" %lu", ((DWORD *)d)[1 + i]);
            o = 4 + 4 * n;
        }
        printf(" str=\"");
        for (const unsigned char *s = *(const unsigned char **)(d + o); *s; s++) printf(*s >= 0x80 ? "\\x%02x" : "%c", *s);
        printf("\" (%s: %lu bytes, a pointer)\n", size == o + sizeof(char *) ? "ok" : "BAD SIZE", o);
        return;
    }
    if (!strcmp(tname, "Words")) {
        printf(" a=%u b=%d c=%g w=%u,%u size=%lu\n", *(WORD *)d, d[2], *(double *)(d + 3), *(WORD *)(d + 11), *(WORD *)(d + 13), size);
        return;
    }
    printf(" size=%lu", size);
    if (!strcmp(tname, "Local")) {
        printf(" a=%lu\n", *(DWORD *)d);
        return;
    }
    for (i = 0; i + 4 <= size; i += 4) printf(" %g", *(float *)(d + i));
    printf("\n");
}

static const char *type_name(const GUID *g) {
    static const struct { DWORD d1; const char *n; } t[] = {
        {0x3D82AB5E, "Vector"}, {0xA42790E1, "TextureFilename"}, {0x3D82AB4D, "Material"},
        {0xF6F23F41, "FrameTransformMatrix"}, {0x3D82AB46, "Frame"}, {0x11223344, NULL}};
    for (int i = 0; t[i].n; i++)
        if (g->Data1 == t[i].d1) return t[i].n;
    if (g->Data1 == 0x11223344) return g->Data4[7] == 0 ? "Numbers" : g->Data4[7] == 1 ? "Words" : "Local";
    return "?";
}

static void dump(IDirectXFileData *d, int depth) {
    char name[64];
    DWORD len = sizeof(name), size;
    const GUID *type;
    GUID id;
    BYTE *data;
    IDirectXFileObject *child;
    HRESULT hr;
    hr = IDirectXFileData_GetName(d, name, &len);
    IDirectXFileData_GetType(d, &type);
    IDirectXFileData_GetId(d, &id);
    hr = IDirectXFileData_GetData(d, NULL, &size, (void **)&data);
    printf("%*s%s '%s' (len %lu, id %08lx) data %#lx", depth * 2, "", type_name(type), name, len, id.Data1, hr);
    dump_data(type_name(type), data, size);
    while ((hr = IDirectXFileData_GetNextObject(d, &child)) == DXFILE_OK) {
        IDirectXFileData *cd;
        IDirectXFileDataReference *r;
        if (SUCCEEDED(IDirectXFileObject_QueryInterface(child, &IID_IDirectXFileData, (void **)&cd))) {
            dump(cd, depth + 1);
            IDirectXFileData_Release(cd);
        } else if (SUCCEEDED(IDirectXFileObject_QueryInterface(child, &IID_IDirectXFileDataReference, (void **)&r))) {
            IDirectXFileData *t;
            len = sizeof(name);
            IDirectXFileDataReference_GetName(r, name, &len);
            hr = IDirectXFileDataReference_Resolve(r, &t);
            printf("%*s{ %s } resolve %#lx", depth * 2 + 2, "", name, hr);
            if (hr == DXFILE_OK) {
                len = sizeof(name);
                IDirectXFileData_GetName(t, name, &len);
                IDirectXFileData_GetType(t, &type);
                printf(" -> %s '%s'", type_name(type), name);
                IDirectXFileData_Release(t);
            }
            printf("\n");
            IDirectXFileDataReference_Release(r);
        }
        IDirectXFileObject_Release(child);
    }
    printf("%*send %#lx\n", depth * 2, "", hr);
}

static void enum_all(IDirectXFile *f, const void *p, DWORD n, const char *label) {
    DXFILELOADMEMORY m = {(void *)p, n};
    IDirectXFileEnumObject *e;
    IDirectXFileData *d;
    HRESULT hr = IDirectXFile_CreateEnumObject(f, &m, DXFILELOAD_FROMMEMORY, &e);
    printf("%s: CreateEnumObject %#lx\n", label, hr);
    if (hr != DXFILE_OK) return;
    while ((hr = IDirectXFileEnumObject_GetNextDataObject(e, &d)) == DXFILE_OK) {
        dump(d, 1);
        IDirectXFileData_Release(d);
    }
    printf("  GetNextDataObject %#lx\n", hr);
    printf("  Release %lu\n", IDirectXFileEnumObject_Release(e));
}

int main(void) {
    HMODULE m = LoadLibraryA("d3dxof.dll");
    IDirectXFile *f;
    HRESULT hr;
    create = (void *)GetProcAddress(m, "DirectXFileCreate");
    printf("DirectXFileCreate %s\n", create ? "found" : "missing");
    if (!create) return 1;
    printf("DirectXFileCreate(NULL) %#lx\n", create(NULL));
    hr = create(&f);
    printf("create %#lx refs %lu\n", hr, refs(f));
    printf("RegisterTemplates %#lx\n", IDirectXFile_RegisterTemplates(f, (void *)templates, sizeof(templates) - 1));
    printf("RegisterTemplates(NULL) %#lx\n", IDirectXFile_RegisterTemplates(f, NULL, 10));
    printf("RegisterTemplates(an object) %#lx\n", IDirectXFile_RegisterTemplates(f, (void *)file_text, sizeof(file_text) - 1));
    enum_all(f, file_text, sizeof(file_text) - 1, "text");
    enum_all(f, file_bin, sizeof(file_bin), "binary");
    enum_all(f, file_zip, sizeof(file_zip) - 1, "compressed");

    {
        /* GetDataObjectByName, GetData of one member, a reference count at each step */
        DXFILELOADMEMORY lm = {(void *)file_text, sizeof(file_text) - 1};
        IDirectXFileEnumObject *e;
        IDirectXFileData *d, *top;
        IDirectXFileObject *c;
        DWORD size;
        ULONG n;
        BYTE *p, *p2;
        IDirectXFile_CreateEnumObject(f, &lm, DXFILELOAD_FROMMEMORY, &e);
        hr = IDirectXFileEnumObject_GetDataObjectByName(e, "Later", &d);
        printf("by name Later %#lx", hr);
        if (hr == DXFILE_OK) {
            hr = IDirectXFileData_GetData(d, "c", &size, (void **)&p);
            printf(" member c %#lx size %lu value %g", hr, size, *(double *)p);
            hr = IDirectXFileData_GetData(d, "w", &size, (void **)&p2);
            printf(" member w %#lx size %lu offset %d", hr, size, (int)(p2 - p));
            printf(" member nope %#lx", IDirectXFileData_GetData(d, "nope", &size, (void **)&p));
            IDirectXFileData_Release(d);
        }
        printf("\nby name Nothing %#lx\n", IDirectXFileEnumObject_GetDataObjectByName(e, "Nothing", &d));
        IDirectXFileEnumObject_Release(e);

        IDirectXFile_CreateEnumObject(f, &lm, DXFILELOAD_FROMMEMORY, &e);
        IDirectXFileEnumObject_GetNextDataObject(e, &top);
        n = refs(top);
        printf("top refs %lu", n);
        n = refs(e);
        printf(" enum refs %lu", n);
        printf(" file refs %lu\n", refs(f));
        IDirectXFileData_GetNextObject(top, &c);
        printf("child refs %lu\n", refs(c));
        /* (one call per statement: gcc evaluates printf's arguments right to left) */
        n = IDirectXFileEnumObject_Release(e);
        printf("release enum %lu", n);
        n = refs(top);
        printf(" -> top %lu", n);
        printf(" child %lu\n", refs(c));
        n = IDirectXFileData_Release(top);
        printf("release top %lu", n);
        printf(" -> child %lu\n", refs(c));
        printf("release child %lu\n", IDirectXFileObject_Release(c));
    }
    {
        /* a file on disk (DXFILELOAD_FROMFILE: a path), a missing one, an unknown template */
        HANDLE h = CreateFileA("xfile-test.x", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
        DWORD w;
        IDirectXFileEnumObject *e;
        IDirectXFileData *d;
        static const char bad[] = "xof 0302txt 0032\nVector { 1;2;3; }\nNoSuch { 1; }\n";
        DXFILELOADMEMORY lm = {(void *)bad, sizeof(bad) - 1};
        WriteFile(h, file_zip, sizeof(file_zip) - 1, &w, NULL);
        CloseHandle(h);
        hr = IDirectXFile_CreateEnumObject(f, "xfile-test.x", DXFILELOAD_FROMFILE, &e);
        printf("from file %#lx", hr);
        if (hr == DXFILE_OK) {
            hr = IDirectXFileEnumObject_GetNextDataObject(e, &d);
            printf(" first %#lx", hr);
            if (hr == DXFILE_OK) IDirectXFileData_Release(d);
            IDirectXFileEnumObject_Release(e);
        }
        DeleteFileA("xfile-test.x");
        printf("\nmissing file %#lx\n", IDirectXFile_CreateEnumObject(f, "no-such-file.x", DXFILELOAD_FROMFILE, &e));
        hr = IDirectXFile_CreateEnumObject(f, &lm, DXFILELOAD_FROMMEMORY, &e);
        printf("unknown template: create %#lx", hr);
        if (hr == DXFILE_OK) {
            hr = IDirectXFileEnumObject_GetNextDataObject(e, &d);
            printf(" first %#lx", hr);
            if (hr == DXFILE_OK) IDirectXFileData_Release(d);
            printf(" second %#lx", IDirectXFileEnumObject_GetNextDataObject(e, &d));
            printf(" third %#lx", IDirectXFileEnumObject_GetNextDataObject(e, &d));
            IDirectXFileEnumObject_Release(e);
        }
        printf("\n");
    }
    printf("release file %lu\n", IDirectXFile_Release(f));
    return 0;
}
