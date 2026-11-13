/* advapi32 accounts and file descriptors as installers and games check a folder's rights
 * (AGS's Aoi5.dll: LookupAccountName "SYSTEM" / "Administrators" / "Users", GetNamedSecurityInfo
 * of the game folder, LookupAccountSid of each ACE, SetNamedSecurityInfo to add what is missing):
 * the SIDs, domains and SID_NAME_USE of the built-in accounts, the buffer-size answers, errors,
 * and the shape of a folder's and a file's descriptor (which parts are there; not the ACEs,
 * which are the machine's). Runs in a scratch folder "wr-acct" in the current directory.
 * libs: -ladvapi32 */
#include "../probe.h"
#include <aclapi.h>
#include <sddl.h>

static void name(const wchar_t *n) {
    BYTE sid[SECURITY_MAX_SID_SIZE];
    wchar_t dom[64], back[64], dom2[64];
    DWORD cb = sizeof sid, cch = 64, cchb = 64, cchd = 64;
    SID_NAME_USE use = 0, use2 = 0;
    SetLastError(0xdead);
    BOOL r = LookupAccountNameW(NULL, n, sid, &cb, dom, &cch, &use);
    printf("LookupAccountNameW(%ls) = %d err %lu", n, r, r ? 0 : GetLastError());
    if (r) {
        char *s = NULL;
        ConvertSidToStringSidA(sid, &s);
        printf(" %s cb %lu dom %ls cch %lu use %d", s, cb, dom, cch, use);
        LocalFree(s);
        r = LookupAccountSidW(NULL, sid, back, &cchb, dom2, &cchd, &use2);
        printf(" | back %d %ls %lu %ls %lu %d", r, back, cchb, dom2, cchd, use2);
    }
    printf("\n");
}

static void sizes(const wchar_t *n) {
    BYTE sid[SECURITY_MAX_SID_SIZE];
    wchar_t dom[64], nm[64];
    DWORD cb = 0, cch = 0;
    SID_NAME_USE use = 0;
    SetLastError(0xdead);
    BOOL r = LookupAccountNameW(NULL, n, NULL, &cb, NULL, &cch, &use);
    printf("sizes %ls: %d err %lu cb %lu cch %lu\n", n, r, GetLastError(), cb, cch);
    cb = 4; cch = 64;
    SetLastError(0xdead);
    r = LookupAccountNameW(NULL, n, sid, &cb, dom, &cch, &use);
    printf("  small sid: %d err %lu cb %lu cch %lu\n", r, GetLastError(), cb, cch);
    cb = sizeof sid; cch = 1;
    SetLastError(0xdead);
    r = LookupAccountNameW(NULL, n, sid, &cb, dom, &cch, &use);
    printf("  small domain: %d err %lu cb %lu cch %lu\n", r, GetLastError(), cb, cch);
    cb = sizeof sid; cch = 64;
    if (LookupAccountNameW(NULL, n, sid, &cb, dom, &cch, &use)) {
        DWORD cn = 0, cd = 0;
        SetLastError(0xdead);
        r = LookupAccountSidW(NULL, sid, NULL, &cn, NULL, &cd, &use);
        printf("  sid sizes: %d err %lu cn %lu cd %lu\n", r, GetLastError(), cn, cd);
        char an[64], ad[64];
        DWORD ca = 64, cda = 64;
        r = LookupAccountSidA(NULL, sid, an, &ca, ad, &cda, &use);
        printf("  A: %d %s %lu %s %lu %d\n", r, an, ca, ad, cda, use);
    }
}

static void shape(const char *what, const wchar_t *path, SECURITY_INFORMATION info) {
    PSID owner = (PSID)1, group = (PSID)1;
    PACL dacl = (PACL)1, sacl = (PACL)1;
    PSECURITY_DESCRIPTOR sd = NULL;
    DWORD e = GetNamedSecurityInfoW(path, SE_FILE_OBJECT, info, &owner, &group, &dacl, &sacl, &sd);
    printf("GetNamedSecurityInfoW(%s, %#lx) = %lu", what, info, e);
    if (!e) {
        SECURITY_DESCRIPTOR_CONTROL ctl = 0;
        DWORD rev = 0;
        GetSecurityDescriptorControl(sd, &ctl, &rev);
        printf(" self-relative %d valid %d owner %s group %s dacl %s sacl %s",
               !!(ctl & SE_SELF_RELATIVE), IsValidSecurityDescriptor(sd),
               owner == (PSID)1 ? "untouched" : owner && IsValidSid(owner) ? "sid" : "null",
               group == (PSID)1 ? "untouched" : group && IsValidSid(group) ? "sid" : "null",
               dacl == (PACL)1 ? "untouched" : dacl && IsValidAcl(dacl) ? "acl" : "null",
               sacl == (PACL)1 ? "untouched" : sacl ? "acl" : "null");
        if (dacl && dacl != (PACL)1) {
            /* every ACE names an account; an ACE for Users or Authenticated Users is there */
            ACL_SIZE_INFORMATION si;
            GetAclInformation(dacl, &si, sizeof si, AclSizeInformation);
            int named = 1, users = 0;
            for (DWORD i = 0; i < si.AceCount; i++) {
                ACCESS_ALLOWED_ACE *a;
                GetAce(dacl, i, (void **)&a);
                wchar_t n[256], d[256];
                DWORD cn = 256, cd = 256;
                SID_NAME_USE u;
                if (!LookupAccountSidW(NULL, &a->SidStart, n, &cn, d, &cd, &u)) named = 0;
                else if (!lstrcmpiW(n, L"Users") || !lstrcmpiW(n, L"Authenticated Users")) users = 1;
            }
            printf(" aces>0 %d named %d users %d", si.AceCount > 0, named, users);
        }
        LocalFree(sd);
    }
    printf("\n");
}

int main(void) {
    const wchar_t *names[] = { L"SYSTEM", L"Administrators", L"Users", L"Everyone", L"Authenticated Users",
        L"NT AUTHORITY\\SYSTEM", L"BUILTIN\\Users", L"users", L"INTERACTIVE", L"Guests", L"CREATOR OWNER",
        L"LOCAL SERVICE", L"NETWORK SERVICE", L"no such account 1234", L"NT AUTHORITY\\Users" };
    for (int i = 0; i < sizeof names / sizeof *names; i++) name(names[i]);
    sizes(L"Users");
    sizes(L"SYSTEM");
    sizes(L"Everyone");

    /* a SID nobody has */
    {
        PSID s;
        ConvertStringSidToSidA("S-1-5-21-1-2-3-4242", &s);
        wchar_t n[64], d[64];
        DWORD cn = 64, cd = 64;
        SID_NAME_USE u;
        SetLastError(0xdead);
        BOOL r = LookupAccountSidW(NULL, s, n, &cn, d, &cd, &u);
        printf("LookupAccountSidW(unknown) = %d err %lu\n", r, GetLastError());
        LocalFree(s);
    }

    CreateDirectoryA("wr-acct", NULL);
    HANDLE f = CreateFileA("wr-acct\\a.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    CloseHandle(f);
    shape("dir", L"wr-acct", DACL_SECURITY_INFORMATION);
    shape("dir", L"wr-acct", DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION);
    shape("dir", L"wr-acct", OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION);
    shape("file", L"wr-acct\\a.txt", OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION);
    shape("missing", L"wr-acct\\missing.txt", DACL_SECURITY_INFORMATION);
    shape("missing dir", L"wr-nodir\\x", DACL_SECURITY_INFORMATION);

    /* what Aoi5.dll does next: a DACL for SYSTEM, Administrators and Users, inherited by what is inside */
    {
        BYTE acl[512];
        InitializeAcl((PACL)acl, sizeof acl, ACL_REVISION);
        const wchar_t *who[] = { L"SYSTEM", L"Administrators", L"Users" };
        for (int i = 0; i < 3; i++) {
            BYTE sid[SECURITY_MAX_SID_SIZE];
            wchar_t dom[64];
            DWORD cb = sizeof sid, cch = 64;
            SID_NAME_USE u;
            LookupAccountNameW(NULL, who[i], sid, &cb, dom, &cch, &u);
            AddAccessAllowedAceEx((PACL)acl, ACL_REVISION, OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE, GENERIC_ALL, sid);
        }
        DWORD e = SetNamedSecurityInfoW(L"wr-acct", SE_FILE_OBJECT, DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION, NULL, NULL, (PACL)acl, NULL);
        printf("SetNamedSecurityInfoW(dir) = %lu\n", e);
        e = SetNamedSecurityInfoW(L"wr-nodir", SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, (PACL)acl, NULL);
        printf("SetNamedSecurityInfoW(missing) = %lu\n", e);
        shape("dir after", L"wr-acct", DACL_SECURITY_INFORMATION);
        /* it was taken (the folder is still writable) */
        f = CreateFileA("wr-acct\\b.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
        printf("write after: %d\n", f != INVALID_HANDLE_VALUE);
        CloseHandle(f);
    }
    /* GetSecurityInfo of a handle */
    {
        HANDLE h = CreateFileA("wr-acct\\a.txt", GENERIC_READ | READ_CONTROL, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
        PACL dacl = NULL;
        PSECURITY_DESCRIPTOR sd = NULL;
        DWORD e = GetSecurityInfo(h, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &dacl, NULL, &sd);
        printf("GetSecurityInfo(file) = %lu dacl %d\n", e, dacl && IsValidAcl(dacl));
        LocalFree(sd);
        CloseHandle(h);
    }
    DeleteFileA("wr-acct\\a.txt");
    DeleteFileA("wr-acct\\b.txt");
    RemoveDirectoryA("wr-acct");
    return 0;
}
