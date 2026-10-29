/* FAKE libpam.so.0 for lock-screen tests only. Never the system PAM stack. */
#include <stdlib.h>
#include <string.h>
#define PAM_SUCCESS 0
#define PAM_SYSTEM_ERR 4
#define PAM_PERM_DENIED 6
#define PAM_AUTH_ERR 7
#define PAM_CONV_ERR 19
#define PAM_DISALLOW_NULL_AUTHTOK 1
#define PAM_REFRESH_CRED 0x10
#define PAM_PROMPT_ECHO_OFF 1
#define PAM_PROMPT_ECHO_ON 2
#define PAM_ERROR_MSG 3
#define PAM_TEXT_INFO 4
struct pam_message { int msg_style; const char *msg; };
struct pam_response { char *resp; int resp_retcode; };
struct pam_conv { int (*conv)(int, const struct pam_message **, struct pam_response **, void *); void *appdata_ptr; };
typedef void pam_handle_t;
static struct pam_conv conversation;
static char fixture_user[256];
const char *fake_pam_marker = "fake-libpam-for-waylock-tests";
int pam_start(const char *service, const char *user, const struct pam_conv *conv, pam_handle_t **p) {
    const char *expected = getenv("FIXTURE_SERVICE") ? getenv("FIXTURE_SERVICE") : "waylock";
    if (strcmp(service, expected) || !user || !*user) return PAM_SYSTEM_ERR;
    strncpy(fixture_user, user, sizeof fixture_user - 1);
    conversation = *conv; *p = (void *)1; return PAM_SUCCESS;
}
static int ask(int style, const char *text, const char *expected) {
    struct pam_message m = {style, text};
    const struct pam_message *ms[] = {&m};
    struct pam_response *reply = NULL;
    if (conversation.conv(1, ms, &reply, conversation.appdata_ptr) != PAM_SUCCESS) return PAM_CONV_ERR;
    int ok = reply && reply[0].resp && !strcmp(reply[0].resp, expected);
    if (reply) { free(reply[0].resp); free(reply); }
    return ok ? PAM_SUCCESS : PAM_AUTH_ERR;
}
int pam_authenticate(pam_handle_t *p, int flags) {
    (void)p;
    if (!(flags & PAM_DISALLOW_NULL_AUTHTOK)) return PAM_SYSTEM_ERR;
    const char *mode = getenv("FIXTURE_MODE") ? getenv("FIXTURE_MODE") : "";
    if (!strcmp(mode, "user")) {   /* report the account the helper chose */
        struct pam_message m = {PAM_TEXT_INFO, fixture_user};
        const struct pam_message *ms[] = {&m};
        struct pam_response *reply = NULL;
        conversation.conv(1, ms, &reply, conversation.appdata_ptr);
        free(reply);
        return PAM_AUTH_ERR;
    }
    if (!strcmp(mode, "too-many") || !strcmp(mode, "max") || !strcmp(mode, "zero")) {
        int n = !strcmp(mode, "too-many") ? 33 : !strcmp(mode, "max") ? 32 : 0;
        struct pam_message m = {PAM_TEXT_INFO, "x"};
        const struct pam_message *ms[33];
        for (int i = 0; i < 33; i++) ms[i] = &m;
        struct pam_response *reply = NULL;
        int r = conversation.conv(n, ms, &reply, conversation.appdata_ptr);
        free(reply);
        return r == PAM_SUCCESS ? PAM_SUCCESS : PAM_CONV_ERR;
    }
    if (!strcmp(mode, "bad-style")) {
        struct pam_message m = {5, "bogus"};
        const struct pam_message *ms[] = {&m};
        struct pam_response *reply = NULL;
        return conversation.conv(1, ms, &reply, conversation.appdata_ptr) == PAM_SUCCESS ? PAM_SUCCESS : PAM_CONV_ERR;
    }
    if (!strcmp(mode, "long")) {
        static char text[5001];
        memset(text, 'm', 5000); text[5000] = 0;
        struct pam_message m = {PAM_ERROR_MSG, text};
        const struct pam_message *ms[] = {&m};
        struct pam_response *reply = NULL;
        int r = conversation.conv(1, ms, &reply, conversation.appdata_ptr);
        free(reply);
        return r == PAM_SUCCESS ? PAM_SUCCESS : PAM_CONV_ERR;
    }
    if (!strcmp(mode, "max-secret")) {
        static char big[4097];
        memset(big, 'x', 4096); big[4096] = 0;
        return ask(PAM_PROMPT_ECHO_OFF, "Password:", big);
    }
    struct pam_message info = {PAM_TEXT_INFO, "Fixture only"};
    struct pam_message password = {PAM_PROMPT_ECHO_OFF, "Password:"};
    const struct pam_message *messages[] = {&info, &password};
    struct pam_response *reply = NULL;
    if (conversation.conv(2, messages, &reply, conversation.appdata_ptr) != PAM_SUCCESS) return PAM_CONV_ERR;
    int ok = reply[1].resp && !strcmp(reply[1].resp, "fixture password") && !reply[0].resp;
    free(reply[1].resp); free(reply);
    if (!ok) return PAM_AUTH_ERR;
    return ask(PAM_PROMPT_ECHO_ON, "Second factor:", "123456");
}
int pam_setcred(pam_handle_t *p, int flags) {
    (void)p;
    return flags == PAM_REFRESH_CRED ? PAM_SUCCESS : PAM_SYSTEM_ERR;
}
int pam_acct_mgmt(pam_handle_t *p, int flags) {
    (void)p; (void)flags;
    return getenv("FIXTURE_ACCOUNT_DENIED") ? PAM_PERM_DENIED : PAM_SUCCESS;
}
int pam_end(pam_handle_t *p, int status) {(void)p; (void)status; return PAM_SUCCESS;}
