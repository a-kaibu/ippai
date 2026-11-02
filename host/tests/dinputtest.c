/* DirectInput 8 keyboard and mouse without input: device creation, state, buffered data
 * (DIERR_NOTBUFFERED before DIPROP_BUFFERSIZE, then an empty queue), the buffer size read back.
 * libs: -ldinput8 -ldxguid */
#define DIRECTINPUT_VERSION 0x0800
#include <dinput.h>
#include <stdio.h>

int main(void)
{
    IDirectInput8A *di;
    IDirectInputDevice8A *kb, *ms;
    DIPROPDWORD p = { { sizeof(p), sizeof(DIPROPHEADER), 0, DIPH_DEVICE }, 0 };
    DIDEVICEOBJECTDATA data[8];
    DIMOUSESTATE2 mst;
    BYTE keys[256];
    DWORD n;
    HRESULT hr;

    hr = DirectInput8Create(GetModuleHandleA(NULL), DIRECTINPUT_VERSION, &IID_IDirectInput8A, (void **)&di, NULL);
    printf("create %lx\n", hr);
    printf("keyboard %lx\n", IDirectInput8_CreateDevice(di, &GUID_SysKeyboard, &kb, NULL));
    printf("mouse %lx\n", IDirectInput8_CreateDevice(di, &GUID_SysMouse, &ms, NULL));
    IDirectInputDevice8_SetDataFormat(kb, &c_dfDIKeyboard);
    IDirectInputDevice8_SetDataFormat(ms, &c_dfDIMouse2);
    n = 8;
    hr = IDirectInputDevice8_GetDeviceData(kb, sizeof(data[0]), data, &n, 0);
    printf("unbuffered %lx %lu\n", hr, n);
    p.dwData = 64;
    printf("set %lx\n", IDirectInputDevice8_SetProperty(kb, DIPROP_BUFFERSIZE, &p.diph));
    p.dwData = 0;
    IDirectInputDevice8_GetProperty(kb, DIPROP_BUFFERSIZE, &p.diph);
    printf("size %lu\n", p.dwData);
    IDirectInputDevice8_Acquire(kb);
    IDirectInputDevice8_Acquire(ms);
    n = 8;
    hr = IDirectInputDevice8_GetDeviceData(kb, sizeof(data[0]), data, &n, 0);
    printf("buffered %lx %lu\n", hr, n);
    printf("keys %lx\n", IDirectInputDevice8_GetDeviceState(kb, sizeof(keys), keys));
    hr = IDirectInputDevice8_GetDeviceState(ms, sizeof(mst), &mst);
    printf("mouse state %lx %ld\n", hr, mst.lZ);
    return 0;
}
