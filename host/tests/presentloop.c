// A Direct3D 9 game that never stops drawing: every frame it rewrites a 512x512 texture
// (1 MiB of new pixels, as a movie or a transition does), draws it over the window and
// presents, as fast as Present lets it. tests/hidden-memory-browser.mjs runs it with the tab
// hidden: the frames the page does not draw must hold the game in Present, not pile up in
// memory (2026-11-14: 金恋 ran out of memory 6-7 minutes after its tab was hidden).
//   i686-w64-mingw32-gcc -O2 host/tests/presentloop.c -o presentloop.exe -mwindows -ld3d9
#include <windows.h>
#include <d3d9.h>
#include <stdio.h>

struct V {
  float x, y, z, rhw, u, v;
};

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
  if (m == WM_DESTROY) PostQuitMessage(0);
  return DefWindowProcA(h, m, w, l);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show) {
  WNDCLASSA wc = {0};
  wc.lpfnWndProc = proc;
  wc.hInstance = inst;
  wc.lpszClassName = "presentloop";
  RegisterClassA(&wc);
  RECT r = {0, 0, 640, 480};
  AdjustWindowRect(&r, WS_OVERLAPPEDWINDOW, FALSE);
  HWND hwnd = CreateWindowA("presentloop", "presentloop", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, r.right - r.left,
                            r.bottom - r.top, NULL, NULL, inst, NULL);
  IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
  D3DPRESENT_PARAMETERS pp = {0};
  pp.Windowed = TRUE;
  pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
  pp.BackBufferFormat = D3DFMT_X8R8G8B8;
  pp.BackBufferWidth = 640;
  pp.BackBufferHeight = 480;
  pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
  IDirect3DDevice9 *dev;
  if (FAILED(d3d->lpVtbl->CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp,
                                       &dev))) {
    printf("presentloop: no device\n");
    return 1;
  }
  IDirect3DTexture9 *tex;
  dev->lpVtbl->CreateTexture(dev, 512, 512, 1, D3DUSAGE_DYNAMIC, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &tex, NULL);
  dev->lpVtbl->SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_TEX1);
  struct V quad[4] = {{0, 0, 0, 1, 0, 0}, {640, 0, 0, 1, 1, 0}, {0, 480, 0, 1, 0, 1}, {640, 480, 0, 1, 1, 1}};
  printf("presentloop: drawing\n");
  fflush(stdout);
  for (unsigned frame = 0;; frame++) {
    MSG msg;
    while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
      if (msg.message == WM_QUIT) return 0;
      DispatchMessageA(&msg);
    }
    D3DLOCKED_RECT lr;
    if (SUCCEEDED(tex->lpVtbl->LockRect(tex, 0, &lr, NULL, D3DLOCK_DISCARD))) {
      for (int y = 0; y < 512; y++) {
        DWORD *row = (DWORD *)((BYTE *)lr.pBits + y * lr.Pitch);
        for (int x = 0; x < 512; x++) row[x] = 0xff000000 | ((x + frame) & 255) << 16 | ((y + frame) & 255) << 8 | (frame & 255);
      }
      tex->lpVtbl->UnlockRect(tex, 0);
    }
    dev->lpVtbl->Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    dev->lpVtbl->BeginScene(dev);
    dev->lpVtbl->SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    dev->lpVtbl->DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, quad, sizeof(struct V));
    dev->lpVtbl->EndScene(dev);
    dev->lpVtbl->Present(dev, NULL, NULL, NULL, NULL);
    if (frame % 600 == 0) {
      printf("presentloop: frame %u\n", frame);
      fflush(stdout);
    }
  }
}
