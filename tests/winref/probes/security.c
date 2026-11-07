/* advapi32's security side as a hardware tool (CPU-Z) uses it before it loads its driver, run
 * as an elevated administrator (the Windows runner's account; sake's model): SIDs, ACLs built
 * with AddAccessAllowedAce / AddAce (also in the middle, also over the free space), security
 * descriptors and AccessCheck (an admins-only DACL lets the process token in, a deny ACE keeps
 * it out), the process token (elevation, Administrators membership), privileges by name and
 * AdjustTokenPrivileges, thread tokens with and without ImpersonateSelf, and the service
 * manager (open, a service that does not exist). Machine-specific values (the user's SID,
 * LUIDs other than the fixed privilege ones) are not printed.
 * libs: -ladvapi32 */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-72s = %lld (err %lu)\n", #expr, r_, GetLastError()); } while (0)

static void acl_info(const char *label, PACL acl) {
    ACL_SIZE_INFORMATION si = { 0 };
    ACL_REVISION_INFORMATION ri = { 0 };
    GetAclInformation(acl, &si, sizeof si, AclSizeInformation);
    GetAclInformation(acl, &ri, sizeof ri, AclRevisionInformation);
    printf("%s: revision %lu aces %lu in use %lu free %lu valid %d\n", label, ri.AclRevision, si.AceCount, si.AclBytesInUse, si.AclBytesFree, IsValidAcl(acl));
    for (DWORD i = 0; i < si.AceCount; i++) {
        ACCESS_ALLOWED_ACE *a;
        if (!GetAce(acl, i, (void **)&a)) continue;
        printf("  ace %lu type %u flags %02x size %u mask %08lx sid-len %lu\n", i, a->Header.AceType, a->Header.AceFlags, a->Header.AceSize, a->Mask,
               GetLengthSid(&a->SidStart));
    }
}

static int check(PSECURITY_DESCRIPTOR sd, HANDLE token, DWORD want) {
    GENERIC_MAPPING gm = { 1, 2, 4, 7 };
    PRIVILEGE_SET ps;
    DWORD psl = sizeof ps, granted = 0xdead;
    BOOL status = 0xdead;
    SetLastError(0xdead);
    BOOL ok = AccessCheck(sd, token, want, &gm, &ps, &psl, &granted, &status);
    printf("  AccessCheck(%lx) -> %d err %lu granted %lx status %d\n", want, ok, ok ? 0 : GetLastError(), granted, status);
    return ok && status;
}

int main(void) {
    SID_IDENTIFIER_AUTHORITY nt = SECURITY_NT_AUTHORITY, world = SECURITY_WORLD_SID_AUTHORITY;
    PSID admins = NULL, everyone = NULL, system = NULL;
    E(AllocateAndInitializeSid(&nt, 2, SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_ADMINS, 0, 0, 0, 0, 0, 0, &admins));
    E(AllocateAndInitializeSid(&world, 1, SECURITY_WORLD_RID, 0, 0, 0, 0, 0, 0, 0, &everyone));
    E(AllocateAndInitializeSid(&nt, 1, SECURITY_LOCAL_SYSTEM_RID, 0, 0, 0, 0, 0, 0, 0, &system));
    E(AllocateAndInitializeSid(&nt, 9, 1, 2, 3, 4, 5, 6, 7, 8, &(PSID){ 0 }));
    printf("lengths %lu %lu %lu valid %d %d\n", GetLengthSid(admins), GetLengthSid(everyone), GetLengthSid(system), IsValidSid(admins), IsValidSid(everyone));
    printf("admins bytes:");
    for (DWORD i = 0; i < GetLengthSid(admins); i++) printf(" %02x", ((BYTE *)admins)[i]);
    printf("\nsubauthorities %u last %lu\n", *GetSidSubAuthorityCount(admins), *GetSidSubAuthority(admins, 1));
    E(EqualSid(admins, everyone));
    BYTE copy[64];
    E(CopySid(sizeof copy, copy, admins));
    E(EqualSid(admins, copy));
    E(CopySid(8, copy, admins));
    E(GetSidLengthRequired(2));

    /* ACLs */
    BYTE buf[256], buf2[256];
    PACL acl = (PACL)buf, acl2 = (PACL)buf2;
    E(InitializeAcl(acl, 7, ACL_REVISION));
    E(InitializeAcl(acl, sizeof buf, 1));
    E(InitializeAcl(acl, sizeof buf, ACL_REVISION));
    acl_info("empty", acl);
    E(AddAccessAllowedAce(acl, ACL_REVISION, GENERIC_ALL, admins));
    E(AddAccessDeniedAce(acl, ACL_REVISION, 2, everyone));
    acl_info("allow admins, deny everyone 2", acl);
    E(InitializeAcl(acl2, sizeof buf2, ACL_REVISION));
    E(AddAccessAllowedAce(acl2, ACL_REVISION, 0x10, system));
    void *ace;
    E(GetAce(acl2, 0, &ace));
    /* AddAce: before the first, at the end (MAXDWORD), past the end */
    E(AddAce(acl, ACL_REVISION, 0, ace, ((ACE_HEADER *)ace)->AceSize));
    E(AddAce(acl, ACL_REVISION, MAXDWORD, ace, ((ACE_HEADER *)ace)->AceSize));
    E(AddAce(acl, ACL_REVISION, 50, ace, ((ACE_HEADER *)ace)->AceSize));
    E(AddAce(acl, ACL_REVISION, 2, ace, ((ACE_HEADER *)ace)->AceSize));
    acl_info("after AddAce", acl);
    E(AddAce(acl, 9, 0, ace, ((ACE_HEADER *)ace)->AceSize));
    E(AddAce(acl, ACL_REVISION, 0, ace, ((ACE_HEADER *)ace)->AceSize - 1));
    {
        BYTE small[40];
        E(InitializeAcl((PACL)small, sizeof small, ACL_REVISION));
        E(AddAccessAllowedAce((PACL)small, ACL_REVISION, 1, admins));
        E(AddAccessAllowedAce((PACL)small, ACL_REVISION, 1, admins));
        E(AddAce((PACL)small, ACL_REVISION, MAXDWORD, ace, ((ACE_HEADER *)ace)->AceSize));
        acl_info("small", (PACL)small);
    }
    /* AddAce with two ACEs at once */
    {
        BYTE two[128];
        E(InitializeAcl((PACL)two, sizeof two, ACL_REVISION));
        ACE_HEADER *first;
        AddAccessAllowedAce(acl2, ACL_REVISION, 0x20, system);
        GetAce(acl2, 0, (void **)&first);
        E(AddAce((PACL)two, ACL_REVISION, MAXDWORD, first, first->AceSize * 2));
        acl_info("two at once", (PACL)two);
    }

    /* the process token */
    HANDLE token = NULL;
    E(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES | TOKEN_DUPLICATE, &token));
    TOKEN_ELEVATION_TYPE et = 0;
    TOKEN_ELEVATION el = { 0 };
    DWORD len = 0;
    E(GetTokenInformation(token, TokenElevationType, &et, sizeof et, &len));
    printf("elevation type %d len %lu\n", et, len);
    E(GetTokenInformation(token, TokenElevation, &el, sizeof el, &len));
    printf("elevated %lu len %lu\n", el.TokenIsElevated, len);
    E(GetTokenInformation(token, TokenGroups, NULL, 0, &len));
    printf("groups need more than a header %d\n", len > sizeof(DWORD));
    BOOL member = 0xdead;
    E(CheckTokenMembership(NULL, admins, &member));
    printf("admins member %d\n", member);
    E(CheckTokenMembership(NULL, everyone, &member));
    printf("everyone member %d\n", member);
    E(CheckTokenMembership(token, admins, &member));

    /* privileges */
    LUID luid = { 0 };
    E(LookupPrivilegeValueA(NULL, "SeLoadDriverPrivilege", &luid));
    printf("SeLoadDriverPrivilege %lu %ld\n", luid.LowPart, luid.HighPart);
    E(LookupPrivilegeValueW(NULL, L"SeDebugPrivilege", &luid));
    printf("SeDebugPrivilege %lu %ld\n", luid.LowPart, luid.HighPart);
    E(LookupPrivilegeValueA(NULL, "seshutdownprivilege", &luid));
    printf("seshutdownprivilege %lu\n", luid.LowPart);
    E(LookupPrivilegeValueA(NULL, "SeNoSuchPrivilege", &luid));
    {
        WCHAR name[64];
        DWORD n = 64;
        luid.LowPart = 10;
        E(LookupPrivilegeNameW(NULL, &luid, name, &n));
        printf("name of 10: %ls (%lu)\n", name, n);
        n = 3;
        E(LookupPrivilegeNameW(NULL, &luid, name, &n));
        printf("needed %lu\n", n);
    }
    {
        TOKEN_PRIVILEGES tp = { 1, { { { 10, 0 }, SE_PRIVILEGE_ENABLED } } };
        TOKEN_PRIVILEGES old = { 0 };
        DWORD ol = 0;
        E(AdjustTokenPrivileges(token, FALSE, &tp, sizeof old, &old, &ol));
        printf("previous count %lu len %lu\n", old.PrivilegeCount, ol);
        E(AdjustTokenPrivileges(token, FALSE, &tp, 0, NULL, NULL));
        tp.Privileges[0].Luid.LowPart = 999;
        E(AdjustTokenPrivileges(token, FALSE, &tp, 0, NULL, NULL));
    }

    /* AccessCheck needs an impersonation token: the process token is not one */
    BYTE sdbuf[SECURITY_DESCRIPTOR_MIN_LENGTH];
    PSECURITY_DESCRIPTOR sd = sdbuf;
    E(InitializeSecurityDescriptor(sd, SECURITY_DESCRIPTOR_REVISION));
    E(IsValidSecurityDescriptor(sd));
    BYTE abuf[64];
    E(InitializeAcl((PACL)abuf, sizeof abuf, ACL_REVISION));
    E(AddAccessAllowedAce((PACL)abuf, ACL_REVISION, 3, admins));
    E(SetSecurityDescriptorDacl(sd, TRUE, (PACL)abuf, FALSE));
    E(SetSecurityDescriptorOwner(sd, admins, FALSE));
    E(SetSecurityDescriptorGroup(sd, admins, FALSE));
    E(IsValidSecurityDescriptor(sd));
    E(GetSecurityDescriptorLength(sd));
    check(sd, token, 1);
    HANDLE thread = (HANDLE)0xdead;
    E(OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, FALSE, &thread));
    printf("thread token without impersonation null %d\n", thread == NULL);
    E(ImpersonateSelf(SecurityImpersonation));
    E(OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, FALSE, &thread));
    printf("admins-only DACL:\n");
    check(sd, thread, 1);
    check(sd, thread, 3);
    check(sd, thread, 4);
    check(sd, thread, MAXIMUM_ALLOWED);
    check(sd, thread, GENERIC_READ);
    E(AddAccessDeniedAce((PACL)abuf, ACL_REVISION, 1, everyone));
    printf("deny everyone 1 after the allow:\n");
    check(sd, thread, 1);
    check(sd, thread, 2);
    /* a deny first wins */
    E(InitializeAcl((PACL)abuf, sizeof abuf, ACL_REVISION));
    E(AddAccessDeniedAce((PACL)abuf, ACL_REVISION, 1, everyone));
    E(AddAccessAllowedAce((PACL)abuf, ACL_REVISION, 3, admins));
    printf("deny everyone 1 first:\n");
    check(sd, thread, 1);
    check(sd, thread, 2);
    check(sd, thread, MAXIMUM_ALLOWED);
    E(SetSecurityDescriptorDacl(sd, TRUE, NULL, FALSE));
    printf("NULL DACL:\n");
    check(sd, thread, 7);
    E(InitializeAcl((PACL)abuf, sizeof abuf, ACL_REVISION));
    E(SetSecurityDescriptorDacl(sd, TRUE, (PACL)abuf, FALSE));
    printf("empty DACL:\n");
    check(sd, thread, 1);
    E(CloseHandle(thread));
    E(RevertToSelf());
    E(OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, FALSE, &thread));

    /* the service manager */
    SC_HANDLE scm = OpenSCManagerA(NULL, NULL, SC_MANAGER_CONNECT);
    printf("OpenSCManagerA ok %d\n", scm != NULL);
    SetLastError(0xdead);
    SC_HANDLE svc = OpenServiceA(scm, "sake_no_such_service_42", SERVICE_ALL_ACCESS);
    printf("OpenServiceA missing %d err %lu\n", svc != NULL, GetLastError());
    SetLastError(0xdead);
    svc = OpenServiceW(scm, L"sake_no_such_service_42", SERVICE_QUERY_STATUS);
    printf("OpenServiceW missing %d err %lu\n", svc != NULL, GetLastError());
    E(CloseServiceHandle(scm));
    E(CloseServiceHandle(NULL));

    FreeSid(admins);
    FreeSid(everyone);
    FreeSid(system);
    E(CloseHandle(token));
    return 0;
}
