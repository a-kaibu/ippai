# Use-case coverage

295 game runs, 3471 entry points they call, 308 the tests call.

| kind | used by games | tested | by games weighted | by calls weighted |
|---|---|---|---|---|
| api | 2924 | 0 | 0.0% | 0.0% |
| d3d | 246 | 118 | 66.8% | 59.3% |
| d3d9 | 132 | 79 | 74.6% | 95.0% |
| glmissing | 1 | 0 | 0.0% | 0.0% |
| stub | 168 | 0 | 0.0% | 0.0% |

## Called by games, reached by no test (3105)

| entry point | games | calls |
|---|---|---|
| `api:kernel32!GetCurrentThreadId` | 284 | 10880410 |
| `api:kernel32!GetProcAddress` | 282 | 61425 |
| `api:kernel32!MultiByteToWideChar` | 274 | 756281 |
| `api:kernel32!WideCharToMultiByte` | 270 | 705363 |
| `api:kernel32!GetFileType` | 263 | 19189 |
| `api:kernel32!GetStdHandle` | 262 | 18175 |
| `api:kernel32!GetACP` | 260 | 695 |
| `api:kernel32!GetCPInfo` | 258 | 735 |
| `api:kernel32!GetEnvironmentStringsW` | 258 | 645 |
| `api:kernel32!QueryPerformanceCounter` | 257 | 20963210 |
| `api:kernel32!FreeEnvironmentStringsW` | 257 | 629 |
| `api:kernel32!SetUnhandledExceptionFilter` | 257 | 442 |
| `api:kernel32!HeapAlloc` | 256 | 1515281 |
| `api:kernel32!CloseHandle` | 256 | 131790 |
| `api:kernel32!GetCurrentProcessId` | 253 | 12965 |
| `api:kernel32!GetCommandLineA` | 252 | 693 |
| `api:kernel32!GetStringTypeW` | 251 | 2111 |
| `api:kernel32!InitializeCriticalSection` | 243 | 193488 |
| `api:kernel32!ReadFile` | 239 | 3703333 |
| `api:kernel32!HeapFree` | 238 | 1245643 |
| `api:user32!GetSystemMetrics` | 233 | 3358382 |
| `api:kernel32!GetModuleFileNameA` | 232 | 11038 |
| `api:kernel32!GetSystemTimeAsFileTime` | 231 | 29508 |
| `api:user32!TranslateMessage` | 226 | 150978 |
| `api:kernel32!GetModuleHandleA` | 226 | 20289 |
| `api:user32!ShowWindow` | 222 | 370 |
| `api:kernel32!CreateThread` | 215 | 11793 |
| `api:kernel32!TlsAlloc` | 210 | 2046 |
| `api:kernel32!TlsSetValue` | 209 | 878718 |
| `api:kernel32!IsProcessorFeaturePresent` | 209 | 644 |
| `api:user32!GetClientRect` | 207 | 172937 |
| `api:user32!GetDC` | 206 | 53369 |
| `api:user32!EndPaint` | 205 | 20626 |
| `api:user32!BeginPaint` | 205 | 20624 |
| `api:kernel32!Sleep` | 203 | 3202878 |
| `api:winmm!timeGetTime` | 201 | 102146208 |
| `api:kernel32!HeapReAlloc` | 200 | 315791 |
| `api:winmm!timeBeginPeriod` | 197 | 301 |
| `api:kernel32!HeapSize` | 196 | 114327 |
| `api:kernel32!InitializeCriticalSectionAndSpinCount` | 196 | 6667 |
| `api:user32!ReleaseDC` | 194 | 53181 |
| `api:user32!GetCursorPos` | 191 | 5228283 |
| `api:ole32!CoInitialize` | 190 | 301 |
| `api:user32!LoadIconA` | 189 | 293 |
| `api:gdi32!GetStockObject` | 184 | 2499 |
| `api:user32!SetWindowPos` | 183 | 2592 |
| `api:sake!IDirectSound8::6` | 183 | 217 |
| `api:kernel32!WriteFile` | 182 | 185719 |
| `api:kernel32!SetFilePointer` | 181 | 569207 |
| `api:kernel32!GetVersionExA` | 181 | 15198 |
| `api:kernel32!GetModuleHandleW` | 181 | 11768 |
| `api:user32!LoadCursorA` | 181 | 1685 |
| `api:sake!IDirectSound8::3` | 181 | 1328 |
| `api:kernel32!WaitForSingleObject` | 180 | 57998100 |
| `api:gdi32!SelectObject` | 179 | 162458 |
| `api:kernel32!CreateEventA` | 179 | 48007 |
| `api:kernel32!LoadLibraryA` | 178 | 2325 |
| `api:kernel32!FlsSetValue` | 177 | 17510 |
| `api:kernel32!FlsAlloc` | 177 | 376 |
| `api:kernel32!FlsGetValue` | 175 | 16403480 |

## Called by games, not implemented (169)

| entry point | games | calls |
|---|---|---|
| `stub:imm32!ImmGetContext` | 86 | 850 |
| `stub:imm32!ImmAssociateContext` | 72 | 79 |
| `stub:user32!WINNLSEnableIME` | 44 | 155 |
| `stub:user32!RegisterTouchWindow` | 41 | 41 |
| `stub:kernel32!InitializeProcThreadAttributeList` | 34 | 68 |
| `stub:kernel32!DeleteProcThreadAttributeList` | 34 | 34 |
| `stub:imm32!ImmReleaseContext` | 31 | 74 |
| `stub:comctl32!FlatSB_SetScrollProp` | 26 | 2196 |
| `stub:comctl32!FlatSB_SetScrollInfo` | 26 | 1098 |
| `stub:comctl32!FlatSB_GetScrollPos` | 26 | 694 |
| `stub:dbghelp!SymGetModuleInfo64` | 25 | 3920 |
| `stub:dbghelp!SymLoadModule64` | 25 | 1670 |
| `stub:dbghelp!SymGetSymFromAddr64` | 25 | 580 |
| `stub:dbghelp!SymGetLineFromAddr64` | 25 | 290 |
| `stub:dbghelp!SymGetOptions` | 25 | 50 |
| `stub:dbghelp!SymGetSearchPath` | 25 | 50 |
| `stub:dbghelp!SymInitialize` | 25 | 50 |
| `stub:dbghelp!SymSetOptions` | 25 | 50 |
| `stub:dbghelp!SymCleanup` | 25 | 25 |
| `stub:comctl32!InitializeFlatSB` | 22 | 147 |
| `stub:dwmapi!DwmSetWindowAttribute` | 21 | 89 |
| `stub:imm32!ImmGetConversionStatus` | 21 | 39 |
| `stub:imm32!ImmGetOpenStatus` | 21 | 21 |
| `stub:user32!RegisterDeviceNotificationW` | 19 | 42 |
| `stub:user32!TrackMouseEvent` | 19 | 24 |
| `stub:imm32!ImmGetDefaultIMEWnd` | 19 | 19 |
| `stub:ole32!CoSetProxyBlanket` | 16 | 17 |
| `stub:setupapi!SetupDiDestroyDeviceInfoList` | 14 | 74 |
| `stub:imm32!ImmSetOpenStatus` | 14 | 35 |
| `stub:setupapi!SetupDiEnumDeviceInterfaces` | 13 | 4137 |
| `stub:setupapi!SetupDiGetClassDevsA` | 13 | 73 |
| `stub:user32!IsTouchWindow` | 12 | 29 |
| `stub:hid!HidD_GetHidGuid` | 11 | 54 |
| `stub:dwmapi!DwmIsCompositionEnabled` | 11 | 13 |
| `stub:dwmapi!DwmGetWindowAttribute` | 10 | 42 |
| `stub:dwmapi!DwmEnableComposition` | 10 | 10 |
| `stub:kernel32!AttachConsole` | 10 | 10 |
| `stub:kernel32!SetConsoleOutputCP` | 10 | 10 |
| `stub:user32!MapVirtualKeyExA` | 9 | 1708 |
| `stub:kernel32!SetConsoleCP` | 9 | 9 |
| `stub:kernel32!GetConsoleScreenBufferInfo` | 8 | 85 |
| `stub:imm32!ImmIsIME` | 8 | 26 |
| `stub:uxtheme!RefreshImmersiveColorPolicyState` | 8 | 18 |
| `stub:uxtheme!SetPreferredAppMode` | 8 | 18 |
| `stub:user32!SwapMouseButton` | 8 | 16 |
| `stub:user32!GetPointerDevices` | 8 | 8 |
| `stub:ole32!CoGetApartmentType` | 7 | 35 |
| `stub:kernel32!SetConsoleMode` | 7 | 7 |
| `stub:user32!GetThreadDesktop` | 7 | 7 |
| `stub:kernel32!FindNextChangeNotification` | 6 | 928149 |
| `stub:kernel32!SetConsoleTextAttribute` | 6 | 148 |
| `stub:user32!ToUnicodeEx` | 6 | 132 |
| `stub:uxtheme!ShouldAppsUseDarkMode` | 6 | 18 |
| `stub:imm32!ImmSetCompositionFontA` | 6 | 9 |
| `stub:avifil32!AVIFileInit` | 6 | 7 |
| `stub:user32!CreateIconFromResourceEx` | 6 | 7 |
| `stub:crypt32!CertOpenSystemStoreA` | 6 | 6 |
| `stub:kernel32!FindFirstChangeNotificationA` | 6 | 6 |
| `stub:kernel32!GetCPInfoExW` | 6 | 6 |
| `stub:kernel32!PowerCreateRequest` | 6 | 6 |
