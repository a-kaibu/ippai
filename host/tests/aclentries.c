/* libs: -ladvapi32 -luser32 */
/* SetEntriesInAcl (installers set folder and key permissions with it) and CharPrevW. */
#include <windows.h>
#include <aclapi.h>
#include <stdio.h>
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
static void dump(const char *label, PACL acl) {
    ACL_SIZE_INFORMATION si;
    GetAclInformation(acl, &si, sizeof si, AclSizeInformation);
    printf("%s: %lu aces", label, si.AceCount);
    for (DWORD i = 0; i < si.AceCount; i++) {
        ACE_HEADER *h; GetAce(acl, i, (void **)&h);
        ACCESS_ALLOWED_ACE *a = (ACCESS_ALLOWED_ACE *)h;
        PSID sid = &a->SidStart;
        printf(" [type %d flags %#x mask %#lx sub %lu]", h->AceType, h->AceFlags, a->Mask,
               *GetSidSubAuthority(sid, *GetSidSubAuthorityCount(sid) - 1));
    }
    printf("\n");
}
int main(void) {
    SID_IDENTIFIER_AUTHORITY world = SECURITY_WORLD_SID_AUTHORITY, nt = SECURITY_NT_AUTHORITY;
    PSID everyone, system;
    AllocateAndInitializeSid(&world, 1, SECURITY_WORLD_RID, 0, 0, 0, 0, 0, 0, 0, &everyone);
    AllocateAndInitializeSid(&nt, 1, SECURITY_LOCAL_SYSTEM_RID, 0, 0, 0, 0, 0, 0, 0, &system);
    EXPLICIT_ACCESS_W e[2] = {0};
    e[0].grfAccessPermissions = GENERIC_READ; e[0].grfAccessMode = GRANT_ACCESS;
    e[0].grfInheritance = SUB_CONTAINERS_AND_OBJECTS_INHERIT;
    e[0].Trustee.TrusteeForm = TRUSTEE_IS_SID; e[0].Trustee.TrusteeType = TRUSTEE_IS_WELL_KNOWN_GROUP;
    e[0].Trustee.ptstrName = (LPWSTR)everyone;
    e[1].grfAccessPermissions = GENERIC_ALL; e[1].grfAccessMode = SET_ACCESS; e[1].grfInheritance = NO_INHERITANCE;
    e[1].Trustee.TrusteeForm = TRUSTEE_IS_SID; e[1].Trustee.ptstrName = (LPWSTR)system;
    PACL acl = NULL;
    CHECK("new ACL", SetEntriesInAclW(2, e, NULL, &acl) == ERROR_SUCCESS && acl && IsValidAcl(acl));
    dump("grant and set", acl);
    EXPLICIT_ACCESS_W d = e[0];
    d.grfAccessMode = DENY_ACCESS; d.grfAccessPermissions = DELETE; d.grfInheritance = NO_INHERITANCE;
    PACL acl2 = NULL;
    CHECK("deny added", SetEntriesInAclW(1, &d, acl, &acl2) == ERROR_SUCCESS && IsValidAcl(acl2));
    dump("deny first", acl2);
    EXPLICIT_ACCESS_W r = e[1]; r.grfAccessMode = REVOKE_ACCESS;
    PACL acl3 = NULL;
    CHECK("revoke", SetEntriesInAclW(1, &r, acl2, &acl3) == ERROR_SUCCESS);
    dump("revoked system", acl3);
    EXPLICIT_ACCESS_W n = {0};
    n.grfAccessPermissions = GENERIC_READ; n.grfAccessMode = GRANT_ACCESS;
    n.Trustee.TrusteeForm = TRUSTEE_IS_NAME; n.Trustee.ptstrName = L"CURRENT_USER";
    PACL acl4 = NULL;
    CHECK("current user by name", SetEntriesInAclW(1, &n, NULL, &acl4) == ERROR_SUCCESS);
    PACL empty = NULL;
    CHECK("no entries", SetEntriesInAclW(0, NULL, NULL, &empty) == ERROR_SUCCESS && empty && IsValidAcl(empty));
    LocalFree(acl); LocalFree(acl2); LocalFree(acl3); LocalFree(acl4); LocalFree(empty);
    WCHAR s[] = L"ab\\c";
    CHECK("CharPrevW", CharPrevW(s, s + 3) == s + 2 && CharPrevW(s, s) == s && CharPrevW(s, s + 1) == s);
    FreeSid(everyone); FreeSid(system);
    return failed;
}
