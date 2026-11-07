/* OpenGL 1.1 through opengl32 (on the Windows runner Microsoft's GDI Generic renderer, no
 * GPU): pixel format choice, clears, immediate-mode triangles and quads (fill rules, smooth and
 * flat shading), textures (RGBA/RGB/LUMINANCE/ALPHA uploads, nearest/linear, repeat/clamp,
 * modulate/replace/decal), blending, alpha test, scissor, depth, glOrtho/viewport transforms,
 * glTexSubImage2D/glCopyTexImage2D, and glReadPixels of a 64x64 window back buffer.
 * libs: -lopengl32 -lgdi32
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#include "../probe.h"
#include <GL/gl.h>

#define W 64
#define H 64

static void shot(const char *name, int tol) {
    static unsigned char px[W * H * 3];
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glReadBuffer(GL_BACK);
    glReadPixels(0, 0, W, H, GL_RGB, GL_UNSIGNED_BYTE, px);
    /* (bottom-up rows to top-down, RGB to the BGRX probe_image takes) */
    static unsigned char out[W * H * 4];
    for (int y = 0; y < H; y++)
        for (int x = 0; x < W; x++) {
            const unsigned char *s = px + ((H - 1 - y) * W + x) * 3;
            unsigned char *d = out + (y * W + x) * 4;
            d[0] = s[2]; d[1] = s[1]; d[2] = s[0]; d[3] = 255;
        }
    GLenum e = glGetError();
    if (e) printf("%s: glGetError %#x\n", name, e);
    probe_image(name, out, W, H, W * 4, tol);
}

static void reset(void) {
    glViewport(0, 0, W, H);
    glMatrixMode(GL_PROJECTION); glLoadIdentity(); glOrtho(0, W, H, 0, -1, 1);
    glMatrixMode(GL_MODELVIEW); glLoadIdentity();
    glDisable(GL_TEXTURE_2D); glDisable(GL_BLEND); glDisable(GL_ALPHA_TEST); glDisable(GL_DEPTH_TEST);
    glDisable(GL_SCISSOR_TEST); glDisable(GL_CULL_FACE);
    glShadeModel(GL_SMOOTH);
    glColor4f(1, 1, 1, 1);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
}

static void quad(float x0, float y0, float x1, float y1, float u0, float v0, float u1, float v1) {
    glBegin(GL_QUADS);
    glTexCoord2f(u0, v0); glVertex2f(x0, y0);
    glTexCoord2f(u1, v0); glVertex2f(x1, y0);
    glTexCoord2f(u1, v1); glVertex2f(x1, y1);
    glTexCoord2f(u0, v1); glVertex2f(x0, y1);
    glEnd();
}

static GLuint tex_of(GLenum fmt, int w, int h, const void *px) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, fmt == GL_RGBA || fmt == GL_RGB ? (int)fmt : (int)fmt, w, h, 0, fmt, GL_UNSIGNED_BYTE, px);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    return t;
}

int main(void) {
    WNDCLASSA wc = { CS_OWNDC, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "gl", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    HDC dc = GetDC(hwnd);
    PIXELFORMATDESCRIPTOR pfd = { sizeof pfd, 1, PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER, PFD_TYPE_RGBA, 32 };
    pfd.cDepthBits = 16;
    int pf = ChoosePixelFormat(dc, &pfd);
    if (!pf || !SetPixelFormat(dc, pf, &pfd)) { printf("skip pixel format %d\n", pf); return 0; }
    PIXELFORMATDESCRIPTOR got;
    DescribePixelFormat(dc, pf, sizeof got, &got);
    printf("pixel format: color %d alpha %d depth %d stencil %d double %d\n", got.cColorBits, got.cAlphaBits >= 8, got.cDepthBits >= 16,
           got.cStencilBits >= 8, !!(got.dwFlags & PFD_DOUBLEBUFFER));
    HGLRC rc = wglCreateContext(dc);
    if (!rc || !wglMakeCurrent(dc, rc)) { printf("skip wglCreateContext\n"); return 0; }
    printf("current %d\n", wglGetCurrentContext() == rc);
    GLint v[4];
    glGetIntegerv(GL_VIEWPORT, v);
    printf("viewport %d %d %d %d\n", v[0], v[1], v[2], v[3]);
    glGetIntegerv(GL_MAX_TEXTURE_SIZE, v);
    printf("max texture >= 1024 %d\n", v[0] >= 1024);

    reset();
    /* clears, scissored */
    glClearColor(0.125f, 0.25f, 0.5f, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glEnable(GL_SCISSOR_TEST);
    glScissor(4, 4, 20, 30);
    glClearColor(1, 0.5f, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glScissor(30, 40, 30, 20);
    glClearColor(0.333f, 0.667f, 0.999f, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    reset();
    shot("clear", 1);

    /* triangles: smooth, flat, pixel-centre edges, a fan with additive blending */
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glBegin(GL_TRIANGLES);
    glColor3f(1, 0, 0); glVertex2f(2, 2);
    glColor3f(0, 1, 0); glVertex2f(40, 10);
    glColor3f(0, 0, 1); glVertex2f(10, 40);
    glEnd();
    glShadeModel(GL_FLAT);
    glBegin(GL_TRIANGLES);
    glColor3f(1, 1, 0); glVertex2f(44, 2);
    glColor3f(0, 1, 1); glVertex2f(62, 2);
    glColor3f(1, 0, 1); glVertex2f(53, 20);
    glEnd();
    glShadeModel(GL_SMOOTH);
    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE);
    glColor3f(0.25f, 0.25f, 0.25f);
    glBegin(GL_TRIANGLE_FAN);
    glVertex2f(30.5f, 46.5f);
    for (int i = 0; i <= 6; i++) {
        float a = i * 6.2831853f / 6;
        glVertex2f(30.5f + 15.25f * __builtin_cosf(a), 46.5f + 15.25f * __builtin_sinf(a));
    }
    glEnd();
    glDisable(GL_BLEND);
    glColor3f(0.5f, 0.5f, 0.5f);
    quad(50, 30, 51, 62, 0, 0, 0, 0);
    quad(52.5f, 30.5f, 54.5f, 62.5f, 0, 0, 0, 0);
    quad(56.25f, 30.75f, 57.75f, 61.25f, 0, 0, 0, 0);
    reset();
    shot("raster", 2);

    /* textures: 4x4 RGBA nearest 1:1 and enlarged, linear, clamp, repeat; env modes */
    unsigned char ck[16 * 4];
    for (int i = 0; i < 16; i++) {
        ck[i * 4] = (unsigned char)((i & 3) * 0x55); ck[i * 4 + 1] = (unsigned char)((i >> 2) * 0x55);
        ck[i * 4 + 2] = (unsigned char)(((i ^ (i >> 2)) & 1) * 0xff); ck[i * 4 + 3] = (unsigned char)(i == 5 ? 0x40 : 0xff);
    }
    GLuint t = tex_of(GL_RGBA, 4, 4, ck);
    glClearColor(0.0625f, 0.0625f, 0.0625f, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glEnable(GL_TEXTURE_2D);
    quad(2, 2, 6, 6, 0, 0, 1, 1);
    quad(8, 2, 32, 26, 0, 0, 1, 1);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP);
    quad(36, 2, 60, 26, 0, 0, 1, 1);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_REPEAT);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_REPEAT);
    quad(2, 32, 30, 60, -1, -1, 2, 2);
    glColor4f(1, 0.5f, 0.25f, 1);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
    quad(34, 32, 46, 44, 0, 0, 1, 1);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_REPLACE);
    quad(48, 32, 60, 44, 0, 0, 1, 1);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_DECAL);
    quad(34, 48, 46, 60, 0, 0, 1, 1);
    reset();
    shot("textures", 2);

    /* formats: RGB, LUMINANCE, ALPHA, LUMINANCE_ALPHA, blended over gray; TexSubImage */
    {
        unsigned char b[64 * 4];
        for (int i = 0; i < 64 * 4; i++) b[i] = (unsigned char)(i * 37 + 11);
        glClearColor(0.5f, 0.5f, 0.5f, 1);
        glClear(GL_COLOR_BUFFER_BIT);
        glEnable(GL_TEXTURE_2D);
        glEnable(GL_BLEND);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        static const GLenum fm[] = { GL_RGB, GL_LUMINANCE, GL_ALPHA, GL_LUMINANCE_ALPHA, GL_RGBA };
        for (int i = 0; i < 5; i++) {
            GLuint u = tex_of(fm[i], 8, 8, b);
            quad(2 + i * 12, 2, 10 + i * 12, 10, 0, 0, 1, 1);
            glDeleteTextures(1, &u);
        }
        GLuint u = tex_of(GL_RGBA, 8, 8, b);
        unsigned char sub[4 * 4 * 4];
        memset(sub, 0xff, sizeof sub);
        for (int i = 0; i < 16; i++) sub[i * 4 + 1] = 0;
        glTexSubImage2D(GL_TEXTURE_2D, 0, 2, 3, 4, 4, GL_RGBA, GL_UNSIGNED_BYTE, sub);
        quad(2, 14, 26, 38, 0, 0, 1, 1);
        glDisable(GL_BLEND);
        /* copy a piece of the frame into a texture and draw it back */
        glCopyTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, 0, H - 16, 16, 16, 0);
        printf("glCopyTexImage2D %#x\n", glGetError());
        quad(32, 14, 64, 46, 0, 0, 1, 1);
        glDeleteTextures(1, &u);
        reset();
        shot("formats", 4);
    }

    /* blending factors, alpha test, depth */
    {
        glClearColor(0, 0, 0, 1);
        glClearDepth(1);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        glBegin(GL_QUADS);
        glColor3f(0, 0, 1); glVertex2f(0, 0);
        glColor3f(1, 0, 0); glVertex2f(64, 0);
        glColor3f(1, 1, 1); glVertex2f(64, 32);
        glColor3f(0, 1, 0); glVertex2f(0, 32);
        glEnd();
        glEnable(GL_BLEND);
        static const GLenum bf[6][2] = { { GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA }, { GL_ONE, GL_ONE }, { GL_DST_COLOR, GL_ZERO },
                                         { GL_ONE_MINUS_DST_COLOR, GL_SRC_COLOR }, { GL_SRC_ALPHA, GL_ONE }, { GL_ZERO, GL_ONE_MINUS_SRC_COLOR } };
        for (int i = 0; i < 6; i++) {
            glBlendFunc(bf[i][0], bf[i][1]);
            glColor4f(0.75f, 0.375f, 0.125f, 0.5f);
            float x = (i % 3) * 21 + 2, y = (i / 3) * 16 + 2;
            quad(x, y, x + 17, y + 12, 0, 0, 0, 0);
        }
        glDisable(GL_BLEND);
        glEnable(GL_ALPHA_TEST);
        glAlphaFunc(GL_GREATER, 0.5f);
        glBegin(GL_QUADS);
        glColor4f(1, 1, 1, 0); glVertex2f(2, 34);
        glColor4f(1, 1, 1, 1); glVertex2f(62, 34);
        glColor4f(1, 1, 1, 1); glVertex2f(62, 44);
        glColor4f(1, 1, 1, 0); glVertex2f(2, 44);
        glEnd();
        glDisable(GL_ALPHA_TEST);
        glEnable(GL_DEPTH_TEST);
        glDepthFunc(GL_LESS);
        glBegin(GL_TRIANGLES);
        glColor3f(1, 0, 0); glVertex3f(2, 46, -0.5f); glVertex3f(40, 46, 0.5f); glVertex3f(2, 63, 0.5f);
        glColor3f(0, 1, 0); glVertex3f(40, 63, -0.5f); glVertex3f(2, 50, 0); glVertex3f(30, 46, 0);
        glEnd();
        reset();
        shot("blend_depth", 4);
    }
    /* transforms: viewport, rotation, scaling on the modelview */
    {
        glClearColor(0, 0, 0, 1);
        glClear(GL_COLOR_BUFFER_BIT);
        glViewport(32, 0, 32, 32);
        glColor3f(1, 1, 0);
        quad(0, 0, 32, 32, 0, 0, 0, 0);
        glViewport(0, 0, W, H);
        glMatrixMode(GL_MODELVIEW);
        glTranslatef(16, 48, 0);
        glRotatef(30, 0, 0, 1);
        glScalef(1.5f, 0.75f, 1);
        glColor3f(0, 1, 1);
        quad(-8, -8, 8, 8, 0, 0, 0, 0);
        reset();
        shot("transforms", 2);
    }
    printf("glGetError %#x\n", glGetError());
    wglMakeCurrent(NULL, NULL);
    printf("wglDeleteContext %d\n", wglDeleteContext(rc));
    return 0;
}
