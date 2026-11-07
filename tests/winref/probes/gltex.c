/* OpenGL 1.1 texture uploads (on the Windows runner Microsoft's GDI Generic renderer): what
 * texels glTexImage2D stores for each source format and type (unsigned, signed and float
 * components, out of range values) into each internal format (RGBA, RGB, LUMINANCE, ALPHA,
 * LUMINANCE_ALPHA, INTENSITY), read back by drawing them 1:1 with GL_REPLACE (colour, then
 * alpha as the colour of a blend over white); the unpack state (alignment, row length, skipped
 * pixels and rows); the errors of bad uploads and pixel store values.
 * libs: -lopengl32 -lgdi32
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#include "../probe.h"
#include <GL/gl.h>

#define W 64
#define H 64
#ifndef GL_BGR_EXT
#define GL_BGR_EXT 0x80E0
#define GL_BGRA_EXT 0x80E1
#endif

static void reset(void) {
    glViewport(0, 0, W, H);
    glMatrixMode(GL_PROJECTION); glLoadIdentity(); glOrtho(0, W, H, 0, -1, 1);
    glMatrixMode(GL_MODELVIEW); glLoadIdentity();
    glDisable(GL_BLEND); glDisable(GL_DEPTH_TEST); glDisable(GL_CULL_FACE);
    glEnable(GL_TEXTURE_2D);
    glColor4f(1, 1, 1, 1);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_REPLACE);
}

static void quad(float x1, float y1) {
    glBegin(GL_QUADS);
    glTexCoord2f(0, 0); glVertex2f(0, 0);
    glTexCoord2f(1, 0); glVertex2f(x1, 0);
    glTexCoord2f(1, 1); glVertex2f(x1, y1);
    glTexCoord2f(0, 1); glVertex2f(0, y1);
    glEnd();
}

/* the top `h` rows, `w` pixels each, of the colour then of the alpha (as hex RRGGBB / AA) */
static void show(const char *label, int w, int h) {
    static unsigned char px[W * 4];
    char line[4096];
    int at = snprintf(line, sizeof line, "%s rgb", label);
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    quad(w, h);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    for (int y = 0; y < h; y++) {
        glReadPixels(0, H - 1 - y, w, 1, GL_RGB, GL_UNSIGNED_BYTE, px);
        for (int x = 0; x < w; x++) at += snprintf(line + at, sizeof line - at, " %02x%02x%02x", px[x * 3], px[x * 3 + 1], px[x * 3 + 2]);
    }
    glClearColor(1, 1, 1, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glEnable(GL_BLEND);
    glBlendFunc(GL_ZERO, GL_SRC_ALPHA);
    quad(w, h);
    glDisable(GL_BLEND);
    at += snprintf(line + at, sizeof line - at, " a");
    for (int y = 0; y < h; y++) {
        glReadPixels(0, H - 1 - y, w, 1, GL_RGB, GL_UNSIGNED_BYTE, px);
        for (int x = 0; x < w; x++) at += snprintf(line + at, sizeof line - at, " %02x", px[x * 3]);
    }
    printf("%s\n", line);
}

static GLuint tex;

static void params(void) {
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
}

static const struct { GLenum e; const char *name; int n; } formats[] = {
    { GL_RED, "red", 1 }, { GL_GREEN, "green", 1 }, { GL_BLUE, "blue", 1 }, { GL_ALPHA, "alpha", 1 },
    { GL_RGB, "rgb", 3 }, { GL_RGBA, "rgba", 4 }, { GL_LUMINANCE, "l", 1 }, { GL_LUMINANCE_ALPHA, "la", 2 },
    { GL_BGR_EXT, "bgr", 3 }, { GL_BGRA_EXT, "bgra", 4 },
};
static const struct { GLenum e; const char *name; } internals[] = {
    { GL_RGBA, "RGBA" }, { GL_RGB, "RGB" }, { GL_LUMINANCE, "L" }, { GL_ALPHA, "A" }, { GL_LUMINANCE_ALPHA, "LA" }, { GL_INTENSITY, "I" },
};

/* 32 components of each type: edges, middles, out of range */
static unsigned char ub[32], sb[32 * 1];
static unsigned short us[32];
static short ss[32];
static unsigned int ui[32];
static int si[32];
static float fl[32];

static void values(void) {
    static const unsigned char u8[] = { 0, 255, 128, 1, 127, 200, 64, 254, 3, 250, 100, 16, 32, 192, 96, 160 };
    static const signed char s8[] = { -128, -1, 0, 1, 64, 127, -64, 126, 63, -127, 32, 100, -2, 2, 120, 50 };
    static const unsigned short u16[] = { 0, 0xffff, 0x00ff, 0x0080, 0x8000, 0x7f80, 0x8080, 0x0100, 0x7fff, 0xff00, 0x0101, 0xfeff, 0x4000, 0xc000, 0x017f, 0x0180 };
    static const short s16[] = { -32768, -1, 0, 1, 0x4000, 0x7fff, 255, 128, -16384, 0x7f00, 0x00ff, 0x0081, 0x3fff, -2, 0x2000, 0x6000 };
    static const unsigned int u32[] = { 0, 0xffffffffu, 0x80000000u, 0x00ffffffu, 0x01000000u, 0x7f800000u, 0x80800000u, 0xff000000u,
                                        0x7fffffffu, 0x40000000u, 0x00800000u, 0xfeffffffu, 0x7f7fffffu, 0xc0000000u, 0x01010101u, 0x80808080u };
    static const int s32[] = { (int)0x80000000, -1, 0, 1, 0x40000000, 0x7fffffff, 0x00ffffff, 0x00800000, (int)0xc0000000, 0x7f000000, 0x3fffffff, 0x01000000, -2, 0x20000000, 0x60000000, 0x7f800000 };
    static const float f[] = { -1, 0, 0.5f, 1, 2, 0.25f, 1 / 255.0f, 0.5f / 255, 0.75f, 254.5f / 255, 0.999f, 0.001f, -0.25f, 1.5f, 0.1f, 0.9f };
    for (int i = 0; i < 32; i++) {
        int k = (i * 7 + i / 16) % 16;
        ub[i] = u8[k]; sb[i] = (unsigned char)s8[k]; us[i] = u16[k]; ss[i] = s16[k]; ui[i] = u32[k]; si[i] = s32[k]; fl[i] = f[k];
    }
}

int main(void) {
    WNDCLASSA wc = { CS_OWNDC, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "gltex", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    HDC dc = GetDC(hwnd);
    PIXELFORMATDESCRIPTOR pfd = { sizeof pfd, 1, PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER, PFD_TYPE_RGBA, 32 };
    int pf = ChoosePixelFormat(dc, &pfd);
    if (!pf || !SetPixelFormat(dc, pf, &pfd)) { printf("skip pixel format %d\n", pf); return 0; }
    HGLRC rc = wglCreateContext(dc);
    if (!rc || !wglMakeCurrent(dc, rc)) { printf("skip wglCreateContext\n"); return 0; }
    reset();
    values();
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    params();
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);

    static const struct { GLenum e; const char *name; const void *p; } types[] = {
        { GL_UNSIGNED_BYTE, "ubyte", ub }, { GL_BYTE, "byte", sb }, { GL_UNSIGNED_SHORT, "ushort", us }, { GL_SHORT, "short", ss },
        { GL_UNSIGNED_INT, "uint", ui }, { GL_INT, "int", si }, { GL_FLOAT, "float", fl },
    };
    for (int t = 0; t < 7; t++)
        for (int f = 0; f < 10; f++)
            for (int i = 0; i < 6; i++) {
                glTexImage2D(GL_TEXTURE_2D, 0, internals[i].e, 8, 1, 0, formats[f].e, types[t].e, types[t].p);
                GLenum e = glGetError();
                char label[64];
                snprintf(label, sizeof label, "tex %s %s %s", types[t].name, formats[f].name, internals[i].name);
                if (e) printf("%s error %#x\n", label, e);
                else show(label, 8, 1);
            }

    /* the unpack state: RGB bytes (rows of 3, 5 or 7 pixels), RGBA floats */
    {
        static unsigned char b[1024];
        for (int i = 0; i < (int)sizeof b; i++) b[i] = (unsigned char)(i * 37 + 11);
        /* (GL 1.1: power of two sizes) */
        static const struct { const char *name; int align, row, skip_px, skip_rows, w; } u[] = {
            { "align1", 1, 0, 0, 0, 2 }, { "align2", 2, 0, 0, 0, 2 }, { "align4", 4, 0, 0, 0, 2 }, { "align8", 8, 0, 0, 0, 2 },
            { "align4_w4", 4, 0, 0, 0, 4 }, { "row5", 1, 5, 0, 0, 2 }, { "row5_align4", 4, 5, 0, 0, 2 }, { "skip_px1", 1, 0, 1, 0, 2 },
            { "skip_rows1", 4, 0, 0, 1, 2 }, { "row7_skip2_1", 2, 7, 2, 1, 2 },
        };
        for (int i = 0; i < 10; i++) {
            glPixelStorei(GL_UNPACK_ALIGNMENT, u[i].align);
            glPixelStorei(GL_UNPACK_ROW_LENGTH, u[i].row);
            glPixelStorei(GL_UNPACK_SKIP_PIXELS, u[i].skip_px);
            glPixelStorei(GL_UNPACK_SKIP_ROWS, u[i].skip_rows);
            glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, u[i].w, 2, 0, GL_RGB, GL_UNSIGNED_BYTE, b);
            char label[64];
            snprintf(label, sizeof label, "unpack %s", u[i].name);
            show(label, u[i].w, 2);
        }
        float fb[64];
        for (int i = 0; i < 64; i++) fb[i] = (float)((i * 37 + 11) % 256) / 255.0f;
        glPixelStorei(GL_UNPACK_ROW_LENGTH, 0); glPixelStorei(GL_UNPACK_SKIP_PIXELS, 0); glPixelStorei(GL_UNPACK_SKIP_ROWS, 0);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 8);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, 1, 4, 0, GL_RGB, GL_FLOAT, fb);
        show("unpack float_rgb_align8", 1, 4);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, 2, 2, 0, GL_LUMINANCE, GL_UNSIGNED_SHORT, b);
        show("unpack ushort_l_align8", 2, 2);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 4, 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, b);
        glTexSubImage2D(GL_TEXTURE_2D, 0, 1, 0, 2, 2, GL_LUMINANCE, GL_UNSIGNED_BYTE, b + 100);
        show("sub l_into_rgba", 4, 2);
    }

    /* errors */
    {
        GLint v = 7;
        glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
        static const int aligns[] = { 0, 3, 16, -1, 2 };
        for (int i = 0; i < 5; i++) {
            glPixelStorei(GL_UNPACK_ALIGNMENT, aligns[i]);
            GLenum e = glGetError();
            glGetIntegerv(GL_UNPACK_ALIGNMENT, &v);
            printf("PixelStore alignment %d: %#x now %d\n", aligns[i], e, v);
        }
        glPixelStorei(GL_UNPACK_ROW_LENGTH, -1);
        GLenum e = glGetError();
        glGetIntegerv(GL_UNPACK_ROW_LENGTH, &v);
        printf("PixelStore row length -1: %#x now %d\n", e, v);
        glPixelStorei(GL_UNPACK_SKIP_ROWS, -1);
        printf("PixelStore skip rows -1: %#x\n", glGetError());
        glPixelStorei(0x1234, 1);
        printf("PixelStore bad pname: %#x\n", glGetError());
        glPixelStorei(GL_UNPACK_ROW_LENGTH, 0); glPixelStorei(GL_UNPACK_SKIP_ROWS, 0);
        unsigned char b[64] = { 0 };
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, -1, 1, 0, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexImage2D width -1: %#x\n", glGetError());
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 3, 1, 0, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexImage2D width 3: %#x\n", glGetError());
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 4, 1, 2, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexImage2D border 2: %#x\n", glGetError());
        glTexImage2D(GL_TEXTURE_2D, -1, GL_RGBA, 4, 1, 0, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexImage2D level -1: %#x\n", glGetError());
        glTexImage2D(GL_TEXTURE_2D, 0, 5, 4, 1, 0, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexImage2D internal 5: %#x\n", glGetError());
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 4, 1, 0, 0x1234, GL_UNSIGNED_BYTE, b);
        printf("TexImage2D format bad: %#x\n", glGetError());
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 4, 1, 0, GL_RGBA, 0x1234, b);
        printf("TexImage2D type bad: %#x\n", glGetError());
        glTexImage2D(0x1234, 0, GL_RGBA, 4, 1, 0, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexImage2D target bad: %#x\n", glGetError());
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 4, 4, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
        printf("TexImage2D NULL: %#x\n", glGetError());
        glTexSubImage2D(GL_TEXTURE_2D, 0, 2, 2, 4, 1, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexSubImage2D past the edge: %#x\n", glGetError());
        glTexSubImage2D(GL_TEXTURE_2D, 0, -1, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexSubImage2D x -1: %#x\n", glGetError());
        glTexSubImage2D(GL_TEXTURE_2D, 1, 0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexSubImage2D undefined level: %#x\n", glGetError());
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 0, 0, GL_RGBA, GL_UNSIGNED_BYTE, b);
        printf("TexSubImage2D empty: %#x\n", glGetError());
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 1, 1, 0x1234, GL_UNSIGNED_BYTE, b);
        printf("TexSubImage2D format bad: %#x\n", glGetError());
        GLint w = 0, h = 0, f = 0;
        glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &w);
        glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &h);
        glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_INTERNAL_FORMAT, &f);
        printf("level 0 after the errors %d x %d internal %#x\n", w, h, f);
    }
    glDeleteTextures(1, &tex);
    wglMakeCurrent(NULL, NULL);
    wglDeleteContext(rc);
    return 0;
}
