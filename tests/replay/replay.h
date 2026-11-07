/* What replay.c (the stream, the calls) shares with the API parts that read pictures back
 * (d3d9img.c, d3d8img.c, d3d11img.c, each with its own D3D header: d3d8.h and d3d9.h do not go
 * together). */
#include <windows.h>

typedef UINT_PTR W;

extern int opt_draws, opt_noimages, opt_tol;
extern int frame;
/* the frame being presented is one to show (-every / -from / -to) */
int frame_wanted(void);

/* a picture in the probe format: from rows of 4-byte pixels */
enum { IMG_BGRX, /* B, G, R, x bytes (D3DFMT_X8R8G8B8, DXGI B8G8R8A8) */
       IMG_RGBX, /* R, G, B, x bytes (DXGI R8G8B8A8) */
       IMG_565, IMG_X555 /* 2-byte pixels */ };
void image_out(const char *name, const void *bits, int w, int h, int pitch, int fmt);
/* (and its alpha as gray, for the 4-byte ones: name_alpha) */
void image_alpha(const char *name, const void *bits, int w, int h, int pitch);

/* hooks: method = "Iface::Method" split; a[0] is the object, n arguments in all; hr the call's
 * result (after) */
void d3d9_before(const char *iface, const char *method, W *a, int n, UINT no);
void d3d9_after(const char *iface, const char *method, W *a, int n, UINT no, HRESULT hr);
void d3d8_before(const char *iface, const char *method, W *a, int n, UINT no);
void d3d8_after(const char *iface, const char *method, W *a, int n, UINT no, HRESULT hr);
void d3d11_before(const char *iface, const char *method, W *a, int n, UINT no);
void d3d11_after(const char *iface, const char *method, W *a, int n, UINT no, HRESULT hr);
