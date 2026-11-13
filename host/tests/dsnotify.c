// IDirectSoundNotify::SetNotificationPositions as streaming games use it, on x86 and x64
// (DSBPOSITIONNOTIFY is 16 bytes on x64, the event at 8: cmvs64.exe, リアライブ 体験版, waited
// for events sake never set and took the silence for a lost device): a looping buffer with four
// positions and a stop position, each event set as the play cursor passes, DSBPN_OFFSETSTOP on
// Stop, and the parameter checks. Also GetDeviceID (cmvs64.exe compares the default playback
// device every frame with the one it opened on): the same answer each time, the device
// DirectSoundEnumerate lists, no capture device. The CI machines have no audio device, so this is not a winref
// probe; the answers are Windows' (Wine's dsound tests).
// libs: -ldsound -ldxguid
#include <windows.h>
#include <mmsystem.h>
#include <dsound.h>
#include <stdio.h>

static GUID listed;
static int listed_n;
static BOOL CALLBACK each(GUID *g, const char *d, const char *m, void *ctx) {
    if (g && !listed_n++) listed = *g;
    return TRUE;
}

int main(void) {
    DirectSoundEnumerateA(each, NULL);
    GUID a = {0}, a2 = {0};
    HRESULT ha = GetDeviceID(&DSDEVID_DefaultPlayback, &a), hb = GetDeviceID(&DSDEVID_DefaultPlayback, &a2);
    printf("GetDeviceID playback %#lx %#lx same %d listed %d\n", ha, hb, !memcmp(&a, &a2, sizeof a), !memcmp(&a, &listed, sizeof a));
    GUID v = {0};
    HRESULT hv = GetDeviceID(&DSDEVID_DefaultVoicePlayback, &v);
    printf("GetDeviceID voice playback %#lx same %d\n", hv, !memcmp(&a, &v, sizeof a));
    printf("GetDeviceID no dest %#lx\n", GetDeviceID(&DSDEVID_DefaultPlayback, NULL));
    IDirectSound8 *ds = NULL;
    HRESULT hr = DirectSoundCreate8(NULL, &ds, NULL);
    printf("DirectSoundCreate8 %#lx\n", hr);
    if (FAILED(hr)) return 0;
    IDirectSound8_SetCooperativeLevel(ds, GetDesktopWindow(), DSSCL_PRIORITY);
    WAVEFORMATEX f = {WAVE_FORMAT_PCM, 2, 44100, 44100 * 4, 4, 16, 0};
    // a quarter of a second, in four parts
    DSBUFFERDESC d = {sizeof d, DSBCAPS_CTRLPOSITIONNOTIFY | DSBCAPS_GETCURRENTPOSITION2 | DSBCAPS_GLOBALFOCUS, 44100, 0, &f, {0}};
    IDirectSoundBuffer *b = NULL;
    printf("CreateSoundBuffer %#lx\n", IDirectSound8_CreateSoundBuffer(ds, &d, &b, NULL));
    IDirectSoundNotify *n = NULL;
    printf("QueryInterface notify %#lx\n", IDirectSoundBuffer_QueryInterface(b, &IID_IDirectSoundNotify, (void **)&n));
    printf("no array %#lx\n", IDirectSoundNotify_SetNotificationPositions(n, 1, NULL));
    HANDLE ev[5];
    DSBPOSITIONNOTIFY pos[5];
    for (int i = 0; i < 5; i++) {
        ev[i] = CreateEventA(NULL, FALSE, FALSE, NULL);
        pos[i].dwOffset = i < 4 ? (DWORD)(d.dwBufferBytes / 4 * i + 4) : DSBPN_OFFSETSTOP;
        pos[i].hEventNotify = ev[i];
    }
    printf("SetNotificationPositions %#lx\n", IDirectSoundNotify_SetNotificationPositions(n, 5, pos));
    printf("Play %#lx\n", IDirectSoundBuffer_Play(b, 0, 0, DSBPLAY_LOOPING));
    // every quarter within two passes of the buffer, in order
    for (int i = 0; i < 4; i++) {
        DWORD r = WaitForSingleObject(ev[i], 2000);
        printf("position %d set %d\n", i, r == WAIT_OBJECT_0);
    }
    // and again on the next loop
    printf("position 0 again %d\n", WaitForSingleObject(ev[0], 2000) == WAIT_OBJECT_0);
    printf("Stop %#lx\n", IDirectSoundBuffer_Stop(b));
    printf("stop event set %d\n", WaitForSingleObject(ev[4], 2000) == WAIT_OBJECT_0);
    IDirectSoundNotify_Release(n);
    IDirectSoundBuffer_Release(b);
    IDirectSound8_Release(ds);
    return 0;
}
