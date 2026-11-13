/* libs: -ladvapi32 */
/* Account names and objects' descriptors, as Softhouse-Chara's AGS5 engine checks its game folder
 * at start (悪魔聖女 体験版: LookupAccountNameW of the user, GetNamedSecurityInfoW of the folder,
 * then its DACL's ACEs; with none it says the folder can't be written and crashes). Only what is
 * the same on every machine is printed (not the user's or the computer's name, nor the SIDs). */
#include <windows.h>
#include <aclapi.h>
#include <stdio.h>
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)

/* Access the DACL's allowed ACEs give the token's groups (deny ACEs first, as Windows walks it) */
static DWORD granted(PACL acl) {
    ACL_SIZE_INFORMATION si;
    DWORD allow = 0, deny = 0;
    if (!GetAclInformation(acl, &si, sizeof si, AclSizeInformation)) return 0;
    for (DWORD i = 0; i < si.AceCount; i++) {
        ACCESS_ALLOWED_ACE *a;
        BOOL in = FALSE;
        if (!GetAce(acl, i, (void **)&a)) return 0;
        if (a->Header.AceFlags & INHERIT_ONLY_ACE) continue;
        if (!CheckTokenMembership(NULL, &a->SidStart, &in) || !in) continue;
        if (a->Header.AceType == ACCESS_DENIED_ACE_TYPE) deny |= a->Mask & ~allow;
        else if (a->Header.AceType == ACCESS_ALLOWED_ACE_TYPE) allow |= a->Mask & ~deny;
    }
    return allow;
}

static void lookup(const WCHAR *name) {
    BYTE sid[SECURITY_MAX_SID_SIZE];
    WCHAR dom[64], back[64], dom2[64];
    DWORD cb = 0, cc = 0, cn, cd;
    SID_NAME_USE use = 0, use2 = 0;
    SetLastError(0xdead);
    BOOL r = LookupAccountNameW(NULL, name, NULL, &cb, NULL, &cc, &use);
    printf("%ls: sizes %d err %lu sid %lu domain %lu\n", name, r, GetLastError(), cb, cc);
    cb = sizeof sid; cc = 64;
    r = LookupAccountNameW(NULL, name, sid, &cb, dom, &cc, &use);
    printf("%ls: %d sid %lu domain \"%ls\" (%lu) use %d\n", name, r, cb, dom, cc, use);
    cn = 0; cd = 0;
    SetLastError(0xdead);
    r = LookupAccountSidW(NULL, sid, NULL, &cn, NULL, &cd, &use2);
    printf("  back: sizes %d err %lu name %lu domain %lu\n", r, GetLastError(), cn, cd);
    cn = 64; cd = 64;
    r = LookupAccountSidW(NULL, sid, back, &cn, dom2, &cd, &use2);
    printf("  back: %d \"%ls\" (%lu) \"%ls\" (%lu) use %d\n", r, back, cn, dom2, cd, use2);
}

int main(void) {
    lookup(L"Everyone");
    lookup(L"Administrators");
    lookup(L"BUILTIN\\Users");
    lookup(L"SYSTEM");
    lookup(L"NT AUTHORITY\\Authenticated Users");
    {
        BYTE sid[SECURITY_MAX_SID_SIZE];
        WCHAR dom[64];
        DWORD cb = sizeof sid, cc = 64;
        SID_NAME_USE use;
        SetLastError(0xdead);
        BOOL r = LookupAccountNameW(NULL, L"no such account 7f3a", sid, &cb, dom, &cc, &use);
        printf("unknown: %d err %lu\n", r, GetLastError());
        char adom[64];
        cb = sizeof sid; cc = sizeof adom;
        r = LookupAccountNameA(NULL, "administrators", sid, &cb, adom, &cc, &use);
        printf("ansi: %d sid %lu domain \"%s\" (%lu) use %d\n", r, cb, adom, cc, use);
    }
    /* the user: by the name GetUserName gives, in the computer's domain; back to the same name,
     * and the token's user */
    {
        WCHAR user[256], comp[256], dom[256], back[256], dom2[256];
        DWORD n = 256, m = 256, cb, cc = 256, cn = 256, cd = 256;
        BYTE sid[SECURITY_MAX_SID_SIZE], tu[256];
        SID_NAME_USE use = 0, use2 = 0;
        GetUserNameW(user, &n);
        GetComputerNameW(comp, &m);
        cb = sizeof sid;
        CHECK("user by name", LookupAccountNameW(NULL, user, sid, &cb, dom, &cc, &use) && use == SidTypeUser);
        CHECK("user's domain is the computer", !lstrcmpiW(dom, comp));
        CHECK("user back by SID", LookupAccountSidW(NULL, sid, back, &cn, dom2, &cd, &use2) && !lstrcmpiW(back, user) && !lstrcmpiW(dom2, comp) && use2 == SidTypeUser);
        HANDLE t;
        DWORD got;
        CHECK("token user", OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &t) && GetTokenInformation(t, TokenUser, tu, sizeof tu, &got) && EqualSid(((TOKEN_USER *)tu)->User.Sid, sid));
        WCHAR q[600];
        wsprintfW(q, L"%s\\%s", comp, user);
        cb = sizeof sid; cc = 256;
        CHECK("COMPUTER\\user", LookupAccountNameW(NULL, q, sid, &cb, dom, &cc, &use) && use == SidTypeUser);
    }
    /* the current folder and a file in it: owner, group and DACL; the process may write */
    {
        WCHAR dir[MAX_PATH];
        GetCurrentDirectoryW(MAX_PATH, dir);
        for (int k = 0; k < 2; k++) {
            WCHAR path[MAX_PATH + 32];
            if (k) {
                wsprintfW(path, L"%s\\acctnames.tmp", dir);
                HANDLE h = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
                CloseHandle(h);
            } else lstrcpyW(path, dir);
            PSID owner = NULL, group = NULL;
            PACL dacl = NULL;
            PSECURITY_DESCRIPTOR sd = NULL;
            DWORD e = GetNamedSecurityInfoW(path, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, &owner, &group, &dacl, NULL, &sd);
            printf("%s: GetNamedSecurityInfoW %lu\n", k ? "file" : "folder", e);
            SECURITY_DESCRIPTOR_CONTROL ctl = 0;
            DWORD rev;
            CHECK("  descriptor", sd && IsValidSecurityDescriptor(sd) && GetSecurityDescriptorControl(sd, &ctl, &rev) && (ctl & SE_SELF_RELATIVE) && (ctl & SE_DACL_PRESENT));
            CHECK("  owner and group", owner && IsValidSid(owner) && group && IsValidSid(group));
            CHECK("  DACL", dacl && IsValidAcl(dacl));
            DWORD g = dacl ? granted(dacl) : 0;
            CHECK("  read, write, delete", (g & (FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE)) == (FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE));
            /* the DACL alone */
            PACL d2 = NULL;
            PSECURITY_DESCRIPTOR sd2 = NULL;
            e = GetNamedSecurityInfoW(path, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &d2, NULL, &sd2);
            CHECK("  DACL alone", e == 0 && d2 && IsValidAcl(d2) && granted(d2) == g);
            /* the AGS5 way: the user's SID by name, an ACL that lets it do everything, set on
             * the object; the object still lets the process write */
            {
                BYTE us[SECURITY_MAX_SID_SIZE], ab[256];
                WCHAR user[256], dom[256];
                DWORD n = 256, cb = sizeof us, cc = 256;
                SID_NAME_USE use;
                GetUserNameW(user, &n);
                LookupAccountNameW(NULL, user, us, &cb, dom, &cc, &use);
                PACL a = (PACL)ab;
                InitializeAcl(a, sizeof ab, ACL_REVISION);
                AddAccessAllowedAceEx(a, ACL_REVISION, k ? 0 : OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE, GENERIC_ALL, us);
                if (k) {
                    e = SetNamedSecurityInfoW(path, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, a, NULL);
                    printf("  SetNamedSecurityInfoW %lu\n", e);
                    PACL d3 = NULL;
                    PSECURITY_DESCRIPTOR sd3 = NULL;
                    e = GetNamedSecurityInfoW(path, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &d3, NULL, &sd3);
                    DWORD g3 = d3 ? granted(d3) : 0;
                    CHECK("  still writable", e == 0 && (g3 & (FILE_GENERIC_WRITE | DELETE)) == (FILE_GENERIC_WRITE | DELETE));
                    LocalFree(sd3);
                }
                WCHAR missing[MAX_PATH + 32];
                wsprintfW(missing, L"%s\\no such file 7f3a.txt", dir);
                printf("  SetNamedSecurityInfoW missing %lu\n", SetNamedSecurityInfoW(missing, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, a, NULL));
            }
            LocalFree(sd);
            LocalFree(sd2);
            if (k) DeleteFileW(path);
        }
        WCHAR path[MAX_PATH + 32];
        wsprintfW(path, L"%s\\no such file 7f3a.txt", dir);
        PACL d = NULL;
        PSECURITY_DESCRIPTOR sd = NULL;
        printf("missing file: %lu\n", GetNamedSecurityInfoW(path, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &d, NULL, &sd));
        char apath[MAX_PATH];
        GetCurrentDirectoryA(MAX_PATH, apath);
        sd = NULL;
        DWORD e = GetNamedSecurityInfoA(apath, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &d, NULL, &sd);
        CHECK("ansi folder", e == 0 && d && IsValidAcl(d));
        LocalFree(sd);
    }
    return failed;
}
