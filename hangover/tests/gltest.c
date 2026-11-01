/* OpenGL check for vnvm: WGL context on a window; a fixed-function textured quad drawn into a
 * framebuffer object (glBegin/glEnd, glTexImage2D, alpha blending), that texture drawn on the
 * window with client arrays, and a spinning triangle from a vertex buffer drawn by a GLSL 1.10
 * program (attributes, a uniform matrix and color). Prints each step and quits after 60 s.
 * Build: i686-w64-mingw32-gcc -O2 gltest.c -o gltest.exe -mwindows -lopengl32 -lgdi32 */
#include <windows.h>
#include <GL/gl.h>
#include <math.h>
#include <stdio.h>

typedef char GLchar;
typedef ptrdiff_t GLsizeiptr;
#define GL_ARRAY_BUFFER 0x8892
#define GL_STATIC_DRAW 0x88E4
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_COMPILE_STATUS 0x8B81
#define GL_LINK_STATUS 0x8B82
#define GL_FRAMEBUFFER 0x8D40
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5

#define FUNCS \
    F(void, glGenBuffers, (GLsizei, GLuint *)) F(void, glBindBuffer, (GLenum, GLuint)) \
    F(void, glBufferData, (GLenum, GLsizeiptr, const void *, GLenum)) F(GLuint, glCreateShader, (GLenum)) \
    F(void, glShaderSource, (GLuint, GLsizei, const GLchar *const *, const GLint *)) F(void, glCompileShader, (GLuint)) \
    F(void, glGetShaderiv, (GLuint, GLenum, GLint *)) F(void, glGetShaderInfoLog, (GLuint, GLsizei, GLsizei *, GLchar *)) \
    F(GLuint, glCreateProgram, (void)) F(void, glAttachShader, (GLuint, GLuint)) F(void, glLinkProgram, (GLuint)) \
    F(void, glGetProgramiv, (GLuint, GLenum, GLint *)) F(void, glUseProgram, (GLuint)) \
    F(void, glBindAttribLocation, (GLuint, GLuint, const GLchar *)) F(GLint, glGetUniformLocation, (GLuint, const GLchar *)) \
    F(void, glUniformMatrix4fv, (GLint, GLsizei, GLboolean, const GLfloat *)) F(void, glUniform4f, (GLint, GLfloat, GLfloat, GLfloat, GLfloat)) \
    F(void, glVertexAttribPointer, (GLuint, GLint, GLenum, GLboolean, GLsizei, const void *)) \
    F(void, glEnableVertexAttribArray, (GLuint)) F(void, glDisableVertexAttribArray, (GLuint)) \
    F(void, glGenFramebuffers, (GLsizei, GLuint *)) F(void, glBindFramebuffer, (GLenum, GLuint)) \
    F(void, glFramebufferTexture2D, (GLenum, GLenum, GLenum, GLuint, GLint)) F(GLenum, glCheckFramebufferStatus, (GLenum))

#define F(r, n, a) static r (WINAPI *p_##n) a;
FUNCS
#undef F

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

static const char *vs_src =
    "attribute vec2 pos;\n"
    "attribute vec3 col;\n"
    "uniform mat4 rot;\n"
    "varying vec3 v_col;\n"
    "void main() { v_col = col; gl_Position = rot * vec4(pos, 0.0, 1.0); }\n";
static const char *fs_src =
    "uniform vec4 tint;\n"
    "varying vec3 v_col;\n"
    "void main() { gl_FragColor = vec4(v_col, 1.0) * tint; }\n";

static GLuint shader(GLenum type, const char *src)
{
    GLuint s = p_glCreateShader(type);
    GLint ok;
    char log[1024];
    p_glShaderSource(s, 1, &src, NULL);
    p_glCompileShader(s);
    p_glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) { p_glGetShaderInfoLog(s, sizeof(log), NULL, log); printf("shader: %s\n", log); }
    printf("compile %#x -> %d\n", type, ok);
    fflush(stdout);
    return s;
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    WNDCLASSA wc = { CS_OWNDC, wndproc, 0, 0, inst, 0, LoadCursor(0, IDC_ARROW), 0, 0, "gltest" };
    PIXELFORMATDESCRIPTOR pfd = { sizeof(pfd), 1, PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER, PFD_TYPE_RGBA, 32 };
    static const float tri[] = { 0.0f, 0.6f, 1, 0, 0, 0.5f, -0.4f, 0, 1, 0, -0.5f, -0.4f, 0, 0, 1 };
    static const float quad[] = { -0.9f, -0.9f, 0, 0, 0.9f, -0.9f, 1, 0, 0.9f, 0.9f, 1, 1, -0.9f, 0.9f, 0, 1 };
    static DWORD px[64 * 64];
    DWORD start = GetTickCount(), frames = 0;
    GLuint tex, rt, fbo, vbo, prog;
    GLint ok, rot, tint;
    HGLRC rc;
    HWND hwnd;
    HDC dc;
    MSG msg;
    int x, y, fmt;

    RegisterClassA(&wc);
    hwnd = CreateWindowA("gltest", "gltest", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 656, 519, 0, 0, inst, 0);
    dc = GetDC(hwnd);
    pfd.cDepthBits = 24;
    fmt = ChoosePixelFormat(dc, &pfd);
    printf("ChoosePixelFormat -> %d, SetPixelFormat -> %d\n", fmt, SetPixelFormat(dc, fmt, &pfd));
    rc = wglCreateContext(dc);
    printf("wglCreateContext -> %p, wglMakeCurrent -> %d\n", rc, wglMakeCurrent(dc, rc));
    if (!rc) return 1;
    printf("GL_VERSION %s, GL_RENDERER %s\n", glGetString(GL_VERSION), glGetString(GL_RENDERER));
#define F(r, n, a) if (!(p_##n = (void *)wglGetProcAddress(#n))) { printf("no %s\n", #n); return 1; }
    FUNCS
#undef F
    fflush(stdout);

    /* checkerboard texture, half transparent dark blue / opaque yellow */
    for (y = 0; y < 64; y++)
        for (x = 0; x < 64; x++) px[y * 64 + x] = ((x / 8 + y / 8) & 1) ? 0xff00ffff : 0x80800000;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 64, 64, 0, GL_RGBA, GL_UNSIGNED_BYTE, px);
    /* render target texture + FBO */
    glGenTextures(1, &rt);
    glBindTexture(GL_TEXTURE_2D, rt);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 256, 256, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    p_glGenFramebuffers(1, &fbo);
    p_glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    p_glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, rt, 0);
    printf("framebuffer status %#x\n", p_glCheckFramebufferStatus(GL_FRAMEBUFFER));
    p_glBindFramebuffer(GL_FRAMEBUFFER, 0);
    /* program + vertex buffer */
    prog = p_glCreateProgram();
    p_glAttachShader(prog, shader(GL_VERTEX_SHADER, vs_src));
    p_glAttachShader(prog, shader(GL_FRAGMENT_SHADER, fs_src));
    p_glBindAttribLocation(prog, 0, "pos");
    p_glBindAttribLocation(prog, 1, "col");
    p_glLinkProgram(prog);
    p_glGetProgramiv(prog, GL_LINK_STATUS, &ok);
    rot = p_glGetUniformLocation(prog, "rot");
    tint = p_glGetUniformLocation(prog, "tint");
    printf("link -> %d, rot %d, tint %d\n", ok, rot, tint);
    p_glGenBuffers(1, &vbo);
    p_glBindBuffer(GL_ARRAY_BUFFER, vbo);
    p_glBufferData(GL_ARRAY_BUFFER, sizeof(tri), tri, GL_STATIC_DRAW);
    p_glBindBuffer(GL_ARRAY_BUFFER, 0);
    printf("glGetError %#x\n", glGetError());
    fflush(stdout);

    while (GetTickCount() - start < 60000)
    {
        float a = (GetTickCount() - start) / 1000.0f;
        float m[16] = { cosf(a), sinf(a), 0, 0, -sinf(a), cosf(a), 0, 0, 0, 0, 1, 0, 0, 0, 0, 1 };
        while (PeekMessageA(&msg, 0, 0, 0, PM_REMOVE))
        {
            if (msg.message == WM_QUIT) return 0;
            DispatchMessageA(&msg);
        }
        /* into the FBO: dark red background, checkerboard quad blended (fixed function, glBegin) */
        p_glBindFramebuffer(GL_FRAMEBUFFER, fbo);
        glViewport(0, 0, 256, 256);
        glClearColor(0.4f, 0.1f, 0.1f, 1);
        glClear(GL_COLOR_BUFFER_BIT);
        glMatrixMode(GL_PROJECTION);
        glLoadIdentity();
        glOrtho(0, 1, 0, 1, -1, 1);
        glMatrixMode(GL_MODELVIEW);
        glLoadIdentity();
        glEnable(GL_BLEND);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        glEnable(GL_TEXTURE_2D);
        glBindTexture(GL_TEXTURE_2D, tex);
        glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
        glColor4f(1, 1, 1, 1);
        glBegin(GL_QUADS);
        glTexCoord2f(0, 0); glVertex2f(0.1f, 0.1f);
        glTexCoord2f(1, 0); glVertex2f(0.9f, 0.1f);
        glTexCoord2f(1, 1); glVertex2f(0.9f, 0.9f);
        glTexCoord2f(0, 1); glVertex2f(0.1f, 0.9f);
        glEnd();
        glDisable(GL_BLEND);
        p_glBindFramebuffer(GL_FRAMEBUFFER, 0);
        /* window: green background, the FBO texture via client arrays (bottom-left corner is red) */
        glViewport(0, 0, 640, 480);
        glClearColor(0.1f, 0.3f, 0.1f, 1);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        glMatrixMode(GL_PROJECTION);
        glLoadIdentity();
        glBindTexture(GL_TEXTURE_2D, rt);
        glEnableClientState(GL_VERTEX_ARRAY);
        glEnableClientState(GL_TEXTURE_COORD_ARRAY);
        glVertexPointer(2, GL_FLOAT, 16, quad);
        glTexCoordPointer(2, GL_FLOAT, 16, quad + 2);
        glDrawArrays(GL_QUADS, 0, 4);
        glDisableClientState(GL_VERTEX_ARRAY);
        glDisableClientState(GL_TEXTURE_COORD_ARRAY);
        glDisable(GL_TEXTURE_2D);
        /* a small fixed-function red square at the bottom left of the window */
        glColor3f(1, 0, 0);
        glBegin(GL_TRIANGLE_FAN);
        glVertex2f(-1, -1); glVertex2f(-0.9f, -1); glVertex2f(-0.9f, -0.9f); glVertex2f(-1, -0.9f);
        glEnd();
        /* the GLSL triangle */
        p_glUseProgram(prog);
        p_glUniformMatrix4fv(rot, 1, GL_FALSE, m);
        p_glUniform4f(tint, 1, 1, 1, 1);
        p_glBindBuffer(GL_ARRAY_BUFFER, vbo);
        p_glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 20, (void *)0);
        p_glVertexAttribPointer(1, 3, GL_FLOAT, GL_FALSE, 20, (void *)8);
        p_glEnableVertexAttribArray(0);
        p_glEnableVertexAttribArray(1);
        glDrawArrays(GL_TRIANGLES, 0, 3);
        p_glDisableVertexAttribArray(0);
        p_glDisableVertexAttribArray(1);
        p_glBindBuffer(GL_ARRAY_BUFFER, 0);
        p_glUseProgram(0);
        SwapBuffers(dc);
        if (++frames % 60 == 0) { printf("frame %lu, glGetError %#x\n", frames, glGetError()); fflush(stdout); }
    }
    printf("done, %lu frames\n", frames);
    return 0;
}
