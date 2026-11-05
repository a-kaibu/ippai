/* OpenGL (WGL): Ren'Py, many doujin engines and ports, at 1280x720. Each case reports ms per frame.
 *
 *   vn                 background, 3 characters, message window as textured quads (glBegin), the
 *                      typing text: GDI into a DIB, glTexSubImage2D
 *   sprites_immediate  2000 particles with glBegin/glEnd each
 *   sprites_arrays     2000 particles from client vertex arrays, one glDrawArrays
 *   upload             a 1280x720 texture rewritten every frame by glTexSubImage2D (movies)
 *   fade               crossfade of two pictures in a GLSL 1.10 fragment shader (two texture units)
 *   rt                 the scene into a framebuffer object, 4 blur passes between two FBOs (GLSL)
 *
 * Build: i686-w64-mingw32-gcc -O2 gl.c -o gl.exe -mwindows -lopengl32 -lgdi32 */
#include "bench.h"
#include <GL/gl.h>

typedef char GLchar;
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_COMPILE_STATUS 0x8B81
#define GL_LINK_STATUS 0x8B82
#define GL_FRAMEBUFFER 0x8D40
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_TEXTURE0 0x84C0
#define GL_TEXTURE1 0x84C1
#define GL_CLAMP_TO_EDGE 0x812F
#define GL_BGRA 0x80E1
#define FUNCS \
    F(GLuint, glCreateShader, (GLenum)) F(void, glShaderSource, (GLuint, GLsizei, const GLchar *const *, const GLint *)) \
    F(void, glCompileShader, (GLuint)) F(void, glGetShaderiv, (GLuint, GLenum, GLint *)) \
    F(void, glGetShaderInfoLog, (GLuint, GLsizei, GLsizei *, GLchar *)) F(GLuint, glCreateProgram, (void)) \
    F(void, glAttachShader, (GLuint, GLuint)) F(void, glLinkProgram, (GLuint)) F(void, glGetProgramiv, (GLuint, GLenum, GLint *)) \
    F(void, glUseProgram, (GLuint)) F(GLint, glGetUniformLocation, (GLuint, const GLchar *)) \
    F(void, glUniform1i, (GLint, GLint)) F(void, glUniform1f, (GLint, GLfloat)) F(void, glUniform2f, (GLint, GLfloat, GLfloat)) \
    F(void, glActiveTexture, (GLenum)) F(void, glGenFramebuffers, (GLsizei, GLuint *)) \
    F(void, glBindFramebuffer, (GLenum, GLuint)) F(void, glFramebufferTexture2D, (GLenum, GLenum, GLenum, GLuint, GLint))
#define F(r, n, a) static r (WINAPI *p_##n) a;
FUNCS
#undef F

static HDC dc;
static GLuint t_bg, t_bg2, t_chr[3], t_win, t_text, t_spr, t_movie, fbo[2], t_rt[2], prog_fade, prog_blur;
static GLint u_k, u_off;
static HDC text_dc;
static unsigned *text_bits, *movie;

static GLuint texture(int w, int h, const unsigned *pixels)
{
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0, GL_BGRA, GL_UNSIGNED_BYTE, pixels);
    return t;
}

/* a quad in pixels (the projection is the window, y down) */
static void draw(GLuint t, float x, float y, float w, float h)
{
    glBindTexture(GL_TEXTURE_2D, t);
    glBegin(GL_TRIANGLE_STRIP);
    glTexCoord2f(0, 0), glVertex2f(x, y);
    glTexCoord2f(1, 0), glVertex2f(x + w, y);
    glTexCoord2f(0, 1), glVertex2f(x, y + h);
    glTexCoord2f(1, 1), glVertex2f(x + w, y + h);
    glEnd();
}

static void scene(void)
{
    int i;
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    for (i = 0; i < 3; i++) draw(t_chr[i], 40 + i * 410, 80, 400, 640);
    draw(t_win, 40, 500, 1200, 200);
}

static void frame_vn(int f)
{
    const WCHAR *line = bench_lines[(f / 40) % BENCH_LINES];
    int n = f % 40 + 1, len = lstrlenW(line), i;
    memset(text_bits, 0, 1024 * 64 * 4);
    ExtTextOutW(text_dc, 0, 16, 0, NULL, line, n < len ? n : len, NULL);
    GdiFlush();
    for (i = 0; i < 1024 * 64; i++) text_bits[i] = (text_bits[i] & 0xff) << 24 | 0xffffff;
    glBindTexture(GL_TEXTURE_2D, t_text);
    glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 1024, 64, GL_BGRA, GL_UNSIGNED_BYTE, text_bits);
    glClear(GL_COLOR_BUFFER_BIT);
    scene();
    draw(t_text, 80, 530, 1024, 64);
    SwapBuffers(dc);
}

static void sprite_xy(int i, int f, float *x, float *y)
{
    *x = (float)((i * 211 + f * (1 + i % 5)) % (SCREEN_W - 32));
    *y = (float)((i * 97 + f * (1 + i % 3)) % (SCREEN_H - 32));
}

static void frame_sprites_immediate(int f)
{
    int i;
    glClear(GL_COLOR_BUFFER_BIT);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    for (i = 0; i < 2000; i++)
    {
        float x, y;
        sprite_xy(i, f, &x, &y);
        glColor4f(1, 1, 1, 1 - (i % 64) / 255.0f);
        draw(t_spr, x, y, 32, 32);
    }
    glColor4f(1, 1, 1, 1);
    SwapBuffers(dc);
}

static float sv[2000 * 4 * 2], st[2000 * 4 * 2];

static void frame_sprites_arrays(int f)
{
    int i;
    glClear(GL_COLOR_BUFFER_BIT);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    for (i = 0; i < 2000; i++)
    {
        float x, y, *v = sv + i * 8;
        sprite_xy(i, f, &x, &y);
        v[0] = x, v[1] = y, v[2] = x + 32, v[3] = y, v[4] = x + 32, v[5] = y + 32, v[6] = x, v[7] = y + 32;
    }
    glBindTexture(GL_TEXTURE_2D, t_spr);
    glEnableClientState(GL_VERTEX_ARRAY);
    glEnableClientState(GL_TEXTURE_COORD_ARRAY);
    glVertexPointer(2, GL_FLOAT, 0, sv);
    glTexCoordPointer(2, GL_FLOAT, 0, st);
    glDrawArrays(GL_QUADS, 0, 2000 * 4);
    glDisableClientState(GL_VERTEX_ARRAY);
    glDisableClientState(GL_TEXTURE_COORD_ARRAY);
    SwapBuffers(dc);
}

static void frame_upload(int f)
{
    pic_movie(movie, SCREEN_W, SCREEN_H, f);
    glBindTexture(GL_TEXTURE_2D, t_movie);
    glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, SCREEN_W, SCREEN_H, GL_BGRA, GL_UNSIGNED_BYTE, movie);
    draw(t_movie, 0, 0, SCREEN_W, SCREEN_H);
    SwapBuffers(dc);
}

static void frame_fade(int f)
{
    p_glUseProgram(prog_fade);
    p_glUniform1f(u_k, (f % 120) / 119.0f);
    p_glActiveTexture(GL_TEXTURE1);
    glBindTexture(GL_TEXTURE_2D, t_bg2);
    p_glActiveTexture(GL_TEXTURE0);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    p_glUseProgram(0);
    SwapBuffers(dc);
}

static void frame_rt(int f)
{
    static const float off[4][2] = { { 1.0f / SCREEN_W, 0 }, { 0, 1.0f / SCREEN_H }, { 2.0f / SCREEN_W, 0 }, { 0, 2.0f / SCREEN_H } };
    int p;
    (void)f;
    p_glBindFramebuffer(GL_FRAMEBUFFER, fbo[0]);
    glClear(GL_COLOR_BUFFER_BIT);
    scene();
    glDisable(GL_BLEND);
    p_glUseProgram(prog_blur);
    for (p = 0; p < 4; p++)
    {
        p_glBindFramebuffer(GL_FRAMEBUFFER, fbo[(p + 1) & 1]);
        p_glUniform2f(u_off, off[p][0], off[p][1]);
        /* (an FBO texture is bottom-up: drawn flipped, which the next pass flips back) */
        draw(t_rt[p & 1], 0, SCREEN_H, SCREEN_W, -SCREEN_H);
    }
    p_glUseProgram(0);
    p_glBindFramebuffer(GL_FRAMEBUFFER, 0);
    draw(t_rt[0], 0, SCREEN_H, SCREEN_W, -SCREEN_H);
    glEnable(GL_BLEND);
    SwapBuffers(dc);
}

static GLuint program(const char *fs)
{
    static const char *vs = "varying vec2 uv;\n"
                            "void main() { uv = gl_MultiTexCoord0.xy; gl_Position = gl_ModelViewProjectionMatrix * gl_Vertex; }\n";
    const char *src[2] = { vs, fs };
    GLenum kinds[2] = { GL_VERTEX_SHADER, GL_FRAGMENT_SHADER };
    GLuint prog = p_glCreateProgram();
    GLint ok;
    char log[512];
    int i;
    for (i = 0; i < 2; i++)
    {
        GLuint s = p_glCreateShader(kinds[i]);
        p_glShaderSource(s, 1, &src[i], NULL);
        p_glCompileShader(s);
        p_glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
        if (!ok)
        {
            p_glGetShaderInfoLog(s, sizeof(log), NULL, log);
            bench_fail(log);
        }
        p_glAttachShader(prog, s);
    }
    p_glLinkProgram(prog);
    p_glGetProgramiv(prog, GL_LINK_STATUS, &ok);
    if (!ok) bench_fail("glLinkProgram");
    return prog;
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    PIXELFORMATDESCRIPTOR pfd = { sizeof(pfd), 1, PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER, PFD_TYPE_RGBA, 32 };
    HWND hwnd;
    HGLRC rc;
    unsigned *p;
    int i;
    (void)inst, (void)prev, (void)cmd, (void)show;
    bench_args();
    hwnd = bench_window("glbench", SCREEN_W, SCREEN_H, NULL);
    dc = GetDC(hwnd);
    if (!SetPixelFormat(dc, ChoosePixelFormat(dc, &pfd), &pfd)) bench_fail("SetPixelFormat");
    rc = wglCreateContext(dc);
    if (!rc || !wglMakeCurrent(dc, rc)) bench_fail("wglCreateContext");
#define F(r, n, a) if (!(p_##n = (void *)wglGetProcAddress(#n))) bench_fail("no " #n);
    FUNCS
#undef F
    glViewport(0, 0, SCREEN_W, SCREEN_H);
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glOrtho(0, SCREEN_W, SCREEN_H, 0, -1, 1);
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();
    glEnable(GL_TEXTURE_2D);
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    glClearColor(0, 0, 0, 1);

    p = pic_alloc(SCREEN_W, SCREEN_H);
    pic_background(p, SCREEN_W, SCREEN_H, 1);
    t_bg = texture(SCREEN_W, SCREEN_H, p);
    pic_background(p, SCREEN_W, SCREEN_H, 2);
    t_bg2 = texture(SCREEN_W, SCREEN_H, p);
    for (i = 0; i < 3; i++)
    {
        pic_character(p, 400, 640, i + 1, 0);
        t_chr[i] = texture(400, 640, p);
    }
    pic_window(p, 1200, 200, 0);
    t_win = texture(1200, 200, p);
    pic_particle(p, 32, 32, 0);
    t_spr = texture(32, 32, p);
    movie = p;
    t_text = texture(1024, 64, NULL);
    t_movie = texture(SCREEN_W, SCREEN_H, NULL);
    for (i = 0; i < 2000; i++)
    {
        float *t = st + i * 8;
        t[0] = 0, t[1] = 0, t[2] = 1, t[3] = 0, t[4] = 1, t[5] = 1, t[6] = 0, t[7] = 1;
    }
    {
        BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), 1024, -64, 1, 32, BI_RGB } };
        text_dc = CreateCompatibleDC(NULL);
        SelectObject(text_dc, CreateDIBSection(text_dc, &bi, DIB_RGB_COLORS, (void **)&text_bits, NULL, 0));
        SelectObject(text_dc, CreateFontW(-32, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, L"MS Gothic"));
        SetBkMode(text_dc, TRANSPARENT);
        SetTextColor(text_dc, RGB(255, 255, 255));
    }

    if (bench_is("vn")) bench_frames("vn", 20, 240, frame_vn);
    if (bench_is("sprites_immediate")) bench_frames("sprites_immediate", 10, 120, frame_sprites_immediate);
    if (bench_is("sprites_arrays")) bench_frames("sprites_arrays", 20, 240, frame_sprites_arrays);
    if (bench_is("upload")) bench_frames("upload", 20, 240, frame_upload);
    if (bench_is("fade"))
    {
        prog_fade = program("uniform sampler2D a, b; uniform float k; varying vec2 uv;\n"
                            "void main() { gl_FragColor = mix(texture2D(a, uv), texture2D(b, uv), k); }\n");
        p_glUseProgram(prog_fade);
        p_glUniform1i(p_glGetUniformLocation(prog_fade, "a"), 0);
        p_glUniform1i(p_glGetUniformLocation(prog_fade, "b"), 1);
        u_k = p_glGetUniformLocation(prog_fade, "k");
        p_glUseProgram(0);
        bench_frames("fade", 20, 240, frame_fade);
    }
    if (bench_is("rt"))
    {
        for (i = 0; i < 2; i++)
        {
            t_rt[i] = texture(SCREEN_W, SCREEN_H, NULL);
            p_glGenFramebuffers(1, &fbo[i]);
            p_glBindFramebuffer(GL_FRAMEBUFFER, fbo[i]);
            p_glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t_rt[i], 0);
        }
        p_glBindFramebuffer(GL_FRAMEBUFFER, 0);
        prog_blur = program("uniform sampler2D a; uniform vec2 off; varying vec2 uv;\n"
                            "void main() { gl_FragColor = 0.5 * (texture2D(a, uv + off) + texture2D(a, uv - off)); }\n");
        p_glUseProgram(prog_blur);
        p_glUniform1i(p_glGetUniformLocation(prog_blur, "a"), 0);
        u_off = p_glGetUniformLocation(prog_blur, "off");
        p_glUseProgram(0);
        bench_frames("rt", 20, 240, frame_rt);
    }
    bench_end();
    return 0;
}
