/* What a Direct3D 11 swap chain shows (d3d11.c, d2d.c): its back buffer copied to a staging
 * texture and mapped, before Present, when bench_frames asks for it. */
static void d3d11_read_back(IDXGISwapChain *sc)
{
    static ID3D11Texture2D *bb, *staging;
    static ID3D11Device *dev;
    static ID3D11DeviceContext *ctx;
    D3D11_MAPPED_SUBRESOURCE m;
    if (!bench_capturing()) return;
    if (!staging)
    {
        D3D11_TEXTURE2D_DESC d;
        BCHECK(IDXGISwapChain_GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&bb));
        ID3D11Texture2D_GetDevice(bb, &dev);
        ID3D11Device_GetImmediateContext(dev, &ctx);
        ID3D11Texture2D_GetDesc(bb, &d);
        d.Usage = D3D11_USAGE_STAGING;
        d.BindFlags = 0;
        d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        d.MiscFlags = 0;
        BCHECK(ID3D11Device_CreateTexture2D(dev, &d, NULL, &staging));
    }
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)bb);
    BCHECK(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &m));
    bench_frame_pixels(m.pData, SCREEN_W, SCREEN_H, m.RowPitch);
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
}
