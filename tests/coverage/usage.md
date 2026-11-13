# Use-case coverage

82 game runs, 2292 entry points they call, 5263 the tests call.

| kind | used by games | tested | by games weighted | by calls weighted |
|---|---|---|---|---|
| api | 1901 | 1029 | 71.5% | 74.4% |
| d3d | 189 | 112 | 70.5% | 93.4% |
| d3d9 | 88 | 67 | 76.6% | 99.1% |
| glmissing | 1 | 0 | 0.0% | 0.0% |
| stub | 113 | 39 | 53.6% | 23.5% |

## Called by games, reached by no test (970)

| entry point | games | calls |
|---|---|---|
| `api:kernel32!InitializeCriticalSectionAndSpinCount` | 61 | 1395 |
| `api:kernel32!InitializeSRWLock` | 48 | 32534 |
| `api:kernel32!EncodePointer` | 44 | 1617 |
| `api:shell32!CommandLineToArgvW` | 44 | 44 |
| `api:sake!d3d11:64:7` | 43 | 84 |
| `api:user32!ClipCursor` | 42 | 4803 |
| `api:sake!d3d11:65:1` | 40 | 72 |
| `d3d:65.1` | 40 | 72 |
| `api:shcore!GetDpiForMonitor` | 39 | 45 |
| `api:dxgi!CreateDXGIFactory2` | 38 | 39 |
| `api:kernel32!GetLogicalProcessorInformationEx` | 35 | 84 |
| `api:kernel32!InitializeConditionVariable` | 34 | 3786 |
| `api:user32!ShowCursor` | 34 | 144 |
| `api:kernel32!CreateSemaphoreExW` | 33 | 2497 |
| `api:kernel32!IsDebuggerPresent` | 33 | 1785 |
| `api:kernel32!CreatePipe` | 33 | 33 |
| `api:kernel32!FindFirstFileExW` | 33 | 33 |
| `api:ntdll!RtlVerifyVersionInfo` | 32 | 47 |
| `api:kernel32!LocaleNameToLCID` | 27 | 2861 |
| `api:sake!d3d11:71:14` | 26 | 772789 |
| `api:sake!d3d11:71:15` | 26 | 772772 |
| `api:sake!d3d11:71:48` | 26 | 574769 |
| `api:sake!d3d11:71:44` | 26 | 51086 |
| `api:ntdll!RtlGrowFunctionTable` | 26 | 31637 |
| `api:sake!d3d11:70:3` | 26 | 20783 |
| `api:sake!d3d11:70:5` | 26 | 4642 |
| `api:sake!d3d11:70:7` | 26 | 3354 |
| `api:sake!d3d11:70:20` | 26 | 672 |
| `api:ntdll!RtlAddGrowableFunctionTable` | 26 | 268 |
| `api:sake!d3d11:70:0` | 26 | 115 |
| `api:kernel32!K32EnumProcessModules` | 26 | 60 |
| `api:ntdll!RtlDeleteGrowableFunctionTable` | 26 | 55 |
| `api:sake!d3d11:74:2` | 25 | 1565929 |
| `api:sake!d3d11:71:33` | 25 | 95978 |
| `api:sake!d3d11:70:15` | 25 | 1614 |
| `api:sake!d3d11:70:23` | 25 | 532 |
| `api:sake!d3d11:70:12` | 25 | 383 |
| `api:kernel32!RtlLookupFunctionEntry` | 25 | 308 |
| `api:sake!d3d11:70:9` | 25 | 207 |
| `api:sake!d3d11:67:9` | 25 | 27 |
| `api:sake!d3d11:64:8` | 25 | 25 |
| `d3d:64.8` | 25 | 25 |
| `api:sake!d3d11:71:8` | 24 | 1771986 |
| `api:sake!d3d11:71:7` | 24 | 202574 |
| `d3d:71.7` | 24 | 202571 |
| `api:sake!d3d11:71:9` | 24 | 146148 |
| `api:sake!d3d11:71:11` | 24 | 133396 |
| `api:sake!d3d11:74:10` | 24 | 17920 |
| `api:sake!d3d11:70:29` | 24 | 1688 |
| `d3d:70.29` | 24 | 1688 |
| `api:sake!d3d11:70:21` | 24 | 271 |
| `api:user32!SetCursor` | 24 | 97 |
| `api:sake!d3d11:71:18` | 23 | 598323 |
| `api:sake!d3d11:71:35` | 23 | 139496 |
| `api:sake!d3d11:71:10` | 23 | 129738 |
| `d3d:71.10` | 23 | 129736 |
| `api:sake!d3d11:71:17` | 23 | 112501 |
| `api:sake!d3d11:71:43` | 23 | 52405 |
| `d3d:71.43` | 23 | 52405 |
| `api:sake!d3d11:71:24` | 23 | 27296 |

## Called by games, not implemented (114)

| entry point | games | calls |
|---|---|---|
| `stub:imm32!ImmGetContext` | 44 | 638 |
| `stub:kernel32!InitializeProcThreadAttributeList` | 33 | 66 |
| `stub:kernel32!DeleteProcThreadAttributeList` | 33 | 33 |
| `stub:user32!RegisterTouchWindow` | 25 | 25 |
| `stub:dbghelp!SymGetModuleInfo64` | 24 | 3850 |
| `stub:dbghelp!SymLoadModule64` | 24 | 1642 |
| `stub:dbghelp!SymGetSymFromAddr64` | 24 | 566 |
| `stub:dbghelp!SymGetLineFromAddr64` | 24 | 283 |
| `stub:dbghelp!SymGetOptions` | 24 | 48 |
| `stub:dbghelp!SymGetSearchPath` | 24 | 48 |
| `stub:dbghelp!SymInitialize` | 24 | 48 |
| `stub:dbghelp!SymSetOptions` | 24 | 48 |
| `stub:dbghelp!SymCleanup` | 24 | 24 |
| `stub:user32!RegisterDeviceNotificationW` | 18 | 38 |
| `stub:imm32!ImmAssociateContext` | 17 | 20 |
| `stub:setupapi!SetupDiDestroyDeviceInfoList` | 12 | 39 |
| `stub:setupapi!SetupDiEnumDeviceInterfaces` | 10 | 37 |
| `stub:setupapi!SetupDiGetClassDevsA` | 10 | 37 |
| `stub:kernel32!SetConsoleOutputCP` | 10 | 10 |
| `stub:dwmapi!DwmGetWindowAttribute` | 9 | 32 |
| `stub:user32!TrackMouseEvent` | 9 | 11 |
| `stub:kernel32!SetConsoleCP` | 9 | 9 |
| `stub:kernel32!GetConsoleScreenBufferInfo` | 8 | 79 |
| `stub:dwmapi!DwmSetWindowAttribute` | 8 | 46 |
| `stub:hid!HidD_GetHidGuid` | 8 | 18 |
| `stub:user32!GetPointerDevices` | 8 | 8 |
| `stub:ole32!CoGetApartmentType` | 7 | 35 |
| `stub:user32!WINNLSEnableIME` | 7 | 13 |
| `stub:uxtheme!RefreshImmersiveColorPolicyState` | 7 | 12 |
| `stub:uxtheme!SetPreferredAppMode` | 7 | 12 |
| `stub:user32!MapVirtualKeyExA` | 6 | 1190 |
| `stub:kernel32!SetConsoleTextAttribute` | 6 | 124 |
| `stub:user32!ToUnicodeEx` | 6 | 112 |
| `stub:uxtheme!ShouldAppsUseDarkMode` | 6 | 18 |
| `stub:user32!CreateIconFromResourceEx` | 6 | 7 |
| `stub:crypt32!CertOpenSystemStoreA` | 6 | 6 |
| `stub:kernel32!AttachConsole` | 6 | 6 |
| `stub:kernel32!GetCPInfoExW` | 6 | 6 |
| `stub:kernel32!PowerCreateRequest` | 6 | 6 |
| `stub:kernel32!PowerSetRequest` | 6 | 6 |
| `stub:kernel32!SetConsoleMode` | 6 | 6 |
| `stub:shell32!SetCurrentProcessExplicitAppUserModelID` | 6 | 6 |
| `stub:uiautomationcore!UiaLookupId` | 6 | 6 |
| `stub:uxtheme!FlushMenuThemes` | 6 | 6 |
| `stub:imm32!ImmCreateContext` | 5 | 5 |
| `stub:ole32!CoSetProxyBlanket` | 5 | 5 |
| `stub:imm32!ImmReleaseContext` | 4 | 5 |
| `glmissing:glDrawBuffers` | 3 | 547 |
| `stub:kernel32!RtlCaptureStackBackTrace` | 3 | 86 |
| `stub:user32!ToUnicode` | 3 | 21 |
| `stub:imm32!ImmNotifyIME` | 3 | 3 |
| `stub:user32!EnableMouseInPointer` | 3 | 3 |
| `stub:user32!GetClipboardSequenceNumber` | 3 | 3 |
| `stub:user32!GetThreadDesktop` | 3 | 3 |
| `stub:user32!IsMouseInPointerEnabled` | 3 | 3 |
| `stub:user32!GetClipCursor` | 2 | 12378 |
| `stub:ucrtbase!iswxdigit` | 2 | 5105 |
| `stub:comctl32!FlatSB_SetScrollProp` | 2 | 172 |
| `stub:comctl32!FlatSB_SetScrollInfo` | 2 | 86 |
| `stub:comctl32!FlatSB_GetScrollPos` | 2 | 62 |
