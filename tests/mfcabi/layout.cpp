// Prints MFC 14 (mfc140.dll, MBCS, x86) class sizes, member offsets and virtual function slots
// for sake's own MFC (crates/sake/src/mfc*.rs): its classes must lay out as the real ones do,
// since the programs built against MFC derive from them. Built and run by
// .github/workflows/mfcabi.yml on a Windows runner (cl /MD /D_AFXDLL).
#include <afxwin.h>
#include <afxdlgs.h>
#include <afxctl.h>
#include <afxcmn.h>
#include <afxole.h>
#include <cstdio>
#include <cstring>

// the vtable slot a pointer to a virtual member function calls through: MSVC makes it a thunk
// "mov eax,[ecx]; jmp [eax+N]" (8B 01 FF 60 nn / FF A0 nnnnnnnn), maybe behind an ILT jmp
template <class F> static int slot(F f) {
  unsigned char *p;
  std::memcpy(&p, &f, sizeof p);
  for (int i = 0; i < 4 && p[0] == 0xE9; i++) p = p + 5 + *(int *)(p + 1);
  if (p[0] == 0x8B && p[1] == 0x01 && p[2] == 0xFF && p[3] == 0x60) return p[4] / 4;
  if (p[0] == 0x8B && p[1] == 0x01 && p[2] == 0xFF && p[3] == 0xA0) return *(int *)(p + 4) / 4;
  if (p[0] == 0x8B && p[1] == 0x01 && p[2] == 0xFF && p[3] == 0x20) return 0;
  return -1;
}
#define SIZE(c) std::printf("size\t%s\t%u\n", #c, (unsigned)sizeof(c))
// (through a derived class: many of the members are protected)
#define OFF(c, m) { struct D : c { static unsigned f() { return (unsigned)offsetof(D, m); } }; \
  std::printf("off\t%s\t%s\t%u\n", #c, #m, D::f()); }
#define VT(c, m) { struct D : c { static int f() { return slot(&D::m); } }; \
  std::printf("vt\t%s\t%s\t%d\n", #c, #m, D::f()); }

int main() {
  SIZE(CObject); SIZE(CCmdTarget); SIZE(CWnd); SIZE(CDialog); SIZE(CDialogEx); SIZE(CWinThread);
  SIZE(CWinApp); SIZE(CString); SIZE(CStringArray); SIZE(CPtrArray); SIZE(CObArray); SIZE(CMapStringToPtr);
  SIZE(CFile); SIZE(CStdioFile); SIZE(CDC); SIZE(CPaintDC); SIZE(CClientDC); SIZE(CGdiObject);
  SIZE(CBitmap); SIZE(CBrush); SIZE(CFont); SIZE(CPen); SIZE(CRgn); SIZE(CButton); SIZE(CEdit);
  SIZE(CStatic); SIZE(CListBox); SIZE(CComboBox); SIZE(CProgressCtrl); SIZE(CImageList);
  SIZE(COleControl); SIZE(COleControlModule); SIZE(COleObjectFactory); SIZE(COlePropertyPage);
  SIZE(CException); SIZE(CMemoryException); SIZE(CFileException); SIZE(CArchive); SIZE(CTime);
  SIZE(CRect); SIZE(CMenu); SIZE(CToolTipCtrl); SIZE(CFrameWnd); SIZE(CFileFind); SIZE(CSingleLock);
  SIZE(CCriticalSection); SIZE(CEvent); SIZE(CMutex); SIZE(AFX_MODULE_STATE); SIZE(AFX_MODULE_THREAD_STATE);
  SIZE(_AFX_THREAD_STATE); SIZE(AFX_MSGMAP); SIZE(AFX_MSGMAP_ENTRY); SIZE(CRuntimeClass); SIZE(CPropertySheet);
  SIZE(CPropertyPage); SIZE(CWaitCursor); SIZE(CStringList); SIZE(CPtrList); SIZE(CObList); SIZE(CMapWordToPtr);
  OFF(CWnd, m_hWnd); OFF(CDialog, m_nIDHelp); OFF(CDialog, m_lpszTemplateName); OFF(CDialog, m_hDialogTemplate);
  OFF(CDialog, m_lpDialogTemplate); OFF(CDialog, m_lpDialogInit); OFF(CDialog, m_pParentWnd); OFF(CDialog, m_hWndTop);
  OFF(CWinThread, m_pMainWnd); OFF(CWinThread, m_pActiveWnd); OFF(CWinThread, m_bAutoDelete); OFF(CWinThread, m_hThread);
  OFF(CWinThread, m_nThreadID); OFF(CWinApp, m_pszAppName); OFF(CWinApp, m_hInstance); OFF(CWinApp, m_lpCmdLine);
  OFF(CWinApp, m_nCmdShow); OFF(CWinApp, m_pszExeName); OFF(CWinApp, m_pszHelpFilePath); OFF(CWinApp, m_pszProfileName);
  OFF(CWinApp, m_pszRegistryKey); OFF(CDC, m_hDC); OFF(CDC, m_hAttribDC); OFF(CPaintDC, m_ps); OFF(CGdiObject, m_hObject);
  OFF(CFile, m_hFile); OFF(CImageList, m_hImageList); OFF(CMenu, m_hMenu);
  VT(CObject, GetRuntimeClass); VT(CObject, Serialize); VT(CCmdTarget, OnCmdMsg); VT(CCmdTarget, OnFinalRelease);
  VT(CCmdTarget, IsInvokeAllowed); VT(CCmdTarget, GetDispatchIID); VT(CCmdTarget, GetTypeInfoCount);
  VT(CCmdTarget, GetTypeLibCache); VT(CCmdTarget, GetTypeLib); VT(CWnd, PreSubclassWindow); VT(CWnd, Create);
  VT(CWnd, DestroyWindow); VT(CWnd, PreCreateWindow); VT(CWnd, CalcWindowRect); VT(CWnd, OnToolHitTest);
  VT(CWnd, GetScrollBarCtrl); VT(CWnd, WinHelpA); VT(CWnd, ContinueModal); VT(CWnd, EndModalLoop);
  VT(CWnd, OnCommand); VT(CWnd, OnNotify); VT(CWnd, GetSuperWndProcAddr); VT(CWnd, DoDataExchange);
  VT(CWnd, BeginModalState); VT(CWnd, EndModalState); VT(CWnd, PreTranslateMessage); VT(CWnd, OnAmbientProperty);
  VT(CWnd, WindowProc); VT(CWnd, OnWndMsg); VT(CWnd, DefWindowProcA); VT(CWnd, PostNcDestroy); VT(CWnd, OnChildNotify);
  VT(CWnd, CheckAutoCenter); VT(CWnd, IsFrameWnd); VT(CWnd, SetOccDialogInfo); VT(CWnd, GetMessageMap);
  VT(CDialog, DoModal); VT(CDialog, OnInitDialog); VT(CDialog, OnSetFont); VT(CDialog, OnOK); VT(CDialog, OnCancel);
  VT(CDialog, PreInitDialog); VT(CWinThread, InitInstance); VT(CWinThread, Run);
  VT(CWinThread, PreTranslateMessage); VT(CWinThread, PumpMessage); VT(CWinThread, OnIdle); VT(CWinThread, IsIdleMessage);
  VT(CWinThread, ExitInstance); VT(CWinThread, ProcessWndProcException); VT(CWinThread, ProcessMessageFilter);
  VT(CWinThread, GetMainWnd); VT(CWinThread, Delete); VT(CWinApp, InitApplication); VT(CWinApp, SaveAllModified);
  VT(CWinApp, DoMessageBox); VT(CWinApp, DoWaitCursor); VT(CWinApp, OnDDECommand); VT(CWinApp, WinHelpA);
  VT(CWinApp, HtmlHelpA); VT(CWinApp, WinHelpInternal); VT(CWinApp, HtmlHelpA); VT(CWinApp, LoadSysPolicies);
  VT(COleControl, OnDraw); VT(COleControl, OnResetState); VT(COleControl, DoPropExchange); VT(COleControl, OnSetClientSite);
  VT(COleControl, GetControlFlags); VT(COleControl, OnGetInPlaceMenu);
  return 0;
}
