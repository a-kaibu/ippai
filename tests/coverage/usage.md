# Use-case coverage

82 game runs, 2337 entry points they call, 5206 the tests call.

| kind | used by games | tested | by games weighted | by calls weighted |
|---|---|---|---|---|
| api | 1930 | 1041 | 71.5% | 83.0% |
| d3d | 191 | 113 | 70.5% | 92.5% |
| d3d9 | 102 | 79 | 79.2% | 99.5% |
| glmissing | 1 | 0 | 0.0% | 0.0% |
| stub | 113 | 39 | 54.0% | 23.7% |

## Called by games, reached by no test (990)

| entry point | games | calls |
|---|---|---|
| `api:kernel32!InitializeCriticalSectionAndSpinCount` | 61 | 1372 |
| `api:kernel32!InitializeSRWLock` | 47 | 32530 |
| `api:kernel32!EncodePointer` | 44 | 1638 |
| `api:shell32!CommandLineToArgvW` | 44 | 44 |
| `api:sake!d3d11:64:7` | 43 | 84 |
| `api:user32!ClipCursor` | 41 | 6364 |
| `api:sake!d3d11:65:1` | 40 | 72 |
| `d3d:65.1` | 40 | 72 |
| `api:shcore!GetDpiForMonitor` | 39 | 45 |
| `api:dxgi!CreateDXGIFactory2` | 38 | 39 |
| `api:user32!ShowCursor` | 35 | 175 |
| `api:kernel32!GetLogicalProcessorInformationEx` | 35 | 84 |
| `api:kernel32!InitializeConditionVariable` | 34 | 3958 |
| `api:kernel32!CreateSemaphoreExW` | 33 | 3866 |
| `api:kernel32!FindFirstFileExW` | 33 | 34 |
| `api:kernel32!CreatePipe` | 33 | 33 |
| `api:kernel32!IsDebuggerPresent` | 32 | 1989 |
| `api:ntdll!RtlVerifyVersionInfo` | 32 | 47 |
| `api:sake!d3d11:71:14` | 26 | 793627 |
| `api:sake!d3d11:71:15` | 26 | 793607 |
| `api:sake!d3d11:71:48` | 26 | 584653 |
| `api:sake!d3d11:71:44` | 26 | 73176 |
| `api:ntdll!RtlGrowFunctionTable` | 26 | 35427 |
| `api:sake!d3d11:70:3` | 26 | 9620 |
| `api:sake!d3d11:70:5` | 26 | 4336 |
| `api:sake!d3d11:70:7` | 26 | 3202 |
| `api:kernel32!LocaleNameToLCID` | 26 | 2860 |
| `api:sake!d3d11:70:20` | 26 | 670 |
| `api:ntdll!RtlAddGrowableFunctionTable` | 26 | 298 |
| `api:sake!d3d11:70:0` | 26 | 115 |
| `api:kernel32!K32EnumProcessModules` | 26 | 60 |
| `api:ntdll!RtlDeleteGrowableFunctionTable` | 26 | 56 |
| `api:sake!d3d11:74:2` | 25 | 1491370 |
| `api:sake!d3d11:71:33` | 25 | 86856 |
| `api:sake!d3d11:70:15` | 25 | 1645 |
| `api:sake!d3d11:70:23` | 25 | 538 |
| `api:sake!d3d11:70:12` | 25 | 423 |
| `api:sake!d3d11:70:9` | 25 | 341 |
| `api:kernel32!RtlLookupFunctionEntry` | 25 | 308 |
| `api:sake!d3d11:67:9` | 25 | 28 |
| `api:sake!d3d11:64:8` | 25 | 25 |
| `d3d:64.8` | 25 | 25 |
| `api:sake!d3d11:71:8` | 24 | 1837379 |
| `api:sake!d3d11:71:7` | 24 | 247254 |
| `d3d:71.7` | 24 | 247254 |
| `api:sake!d3d11:71:9` | 24 | 155858 |
| `api:sake!d3d11:71:11` | 24 | 135283 |
| `api:sake!d3d11:74:10` | 24 | 15977 |
| `api:sake!d3d11:70:29` | 24 | 1688 |
| `d3d:70.29` | 24 | 1688 |
| `api:sake!d3d11:70:21` | 24 | 279 |
| `api:sake!d3d11:71:18` | 23 | 605518 |
| `api:sake!d3d11:71:10` | 23 | 146551 |
| `d3d:71.10` | 23 | 146549 |
| `api:sake!d3d11:71:35` | 23 | 125138 |
| `api:sake!d3d11:71:17` | 23 | 114163 |
| `api:sake!d3d11:71:43` | 23 | 62144 |
| `d3d:71.43` | 23 | 62142 |
| `api:sake!d3d11:71:24` | 23 | 29478 |
| `api:sake!d3d11:71:50` | 23 | 17274 |

## Called by games, not implemented (114)

| entry point | games | calls |
|---|---|---|
| `stub:imm32!ImmGetContext` | 43 | 683 |
| `stub:kernel32!InitializeProcThreadAttributeList` | 33 | 66 |
| `stub:kernel32!DeleteProcThreadAttributeList` | 33 | 33 |
| `stub:dbghelp!SymGetModuleInfo64` | 24 | 3850 |
| `stub:dbghelp!SymLoadModule64` | 24 | 1642 |
| `stub:dbghelp!SymGetSymFromAddr64` | 24 | 566 |
| `stub:dbghelp!SymGetLineFromAddr64` | 24 | 283 |
| `stub:dbghelp!SymGetOptions` | 24 | 48 |
| `stub:dbghelp!SymGetSearchPath` | 24 | 48 |
| `stub:dbghelp!SymInitialize` | 24 | 48 |
| `stub:dbghelp!SymSetOptions` | 24 | 48 |
| `stub:dbghelp!SymCleanup` | 24 | 24 |
| `stub:user32!RegisterTouchWindow` | 24 | 24 |
| `stub:user32!RegisterDeviceNotificationW` | 17 | 37 |
| `stub:imm32!ImmAssociateContext` | 16 | 18 |
| `stub:setupapi!SetupDiDestroyDeviceInfoList` | 11 | 39 |
| `stub:kernel32!SetConsoleOutputCP` | 10 | 10 |
| `stub:dwmapi!DwmGetWindowAttribute` | 9 | 37 |
| `stub:setupapi!SetupDiEnumDeviceInterfaces` | 9 | 37 |
| `stub:setupapi!SetupDiGetClassDevsA` | 9 | 37 |
| `stub:user32!TrackMouseEvent` | 9 | 12 |
| `stub:kernel32!SetConsoleCP` | 9 | 9 |
| `stub:kernel32!GetConsoleScreenBufferInfo` | 8 | 64 |
| `stub:dwmapi!DwmSetWindowAttribute` | 8 | 46 |
| `stub:hid!HidD_GetHidGuid` | 8 | 20 |
| `stub:user32!WINNLSEnableIME` | 8 | 14 |
| `stub:user32!GetPointerDevices` | 8 | 8 |
| `stub:user32!MapVirtualKeyExA` | 7 | 1208 |
| `stub:ole32!CoGetApartmentType` | 7 | 35 |
| `stub:uxtheme!RefreshImmersiveColorPolicyState` | 7 | 12 |
| `stub:uxtheme!SetPreferredAppMode` | 7 | 12 |
| `stub:kernel32!SetConsoleTextAttribute` | 6 | 112 |
| `stub:uxtheme!ShouldAppsUseDarkMode` | 6 | 18 |
| `stub:kernel32!AttachConsole` | 6 | 6 |
| `stub:kernel32!GetCPInfoExW` | 6 | 6 |
| `stub:kernel32!PowerCreateRequest` | 6 | 6 |
| `stub:kernel32!PowerSetRequest` | 6 | 6 |
| `stub:kernel32!SetConsoleMode` | 6 | 6 |
| `stub:shell32!SetCurrentProcessExplicitAppUserModelID` | 6 | 6 |
| `stub:uiautomationcore!UiaLookupId` | 6 | 6 |
| `stub:uxtheme!FlushMenuThemes` | 6 | 6 |
| `stub:user32!ToUnicodeEx` | 5 | 124 |
| `stub:user32!CreateIconFromResourceEx` | 5 | 6 |
| `stub:crypt32!CertOpenSystemStoreA` | 5 | 5 |
| `stub:imm32!ImmCreateContext` | 5 | 5 |
| `stub:ole32!CoSetProxyBlanket` | 5 | 5 |
| `glmissing:glDrawBuffers` | 3 | 658 |
| `stub:kernel32!RtlCaptureStackBackTrace` | 3 | 30 |
| `stub:imm32!ImmReleaseContext` | 3 | 4 |
| `stub:imm32!ImmNotifyIME` | 3 | 3 |
| `stub:user32!EnableMouseInPointer` | 3 | 3 |
| `stub:user32!GetThreadDesktop` | 3 | 3 |
| `stub:user32!IsMouseInPointerEnabled` | 3 | 3 |
| `stub:ucrtbase!iswxdigit` | 2 | 5105 |
| `stub:comctl32!FlatSB_SetScrollProp` | 2 | 172 |
| `stub:comctl32!FlatSB_SetScrollInfo` | 2 | 86 |
| `stub:comctl32!FlatSB_GetScrollPos` | 2 | 62 |
| `stub:ucrtbase!iswalnum` | 2 | 55 |
| `stub:user32!ToUnicode` | 2 | 11 |
| `stub:kernel32!CheckRemoteDebuggerPresent` | 2 | 10 |
