/* Wayland OSK bridge. Never grabs hardware input or exports surrounding text.
 * stdin: key <evdev> <flags>, text <Unicode scalar>, layout <xkb-layout>.
 * stdout: capabilities, layout labels and text-field focus metadata only.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <locale.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include <xkbcommon/xkbcommon-compose.h>
#include "input-method-unstable-v2.h"
#include "virtual-keyboard-unstable-v1.h"

static struct wl_display *display;
static struct wl_seat *seat;
static struct zwp_input_method_manager_v2 *im_manager;
static struct zwp_virtual_keyboard_manager_v1 *vk_manager;
static struct zwp_input_method_v2 *im;
static struct zwp_virtual_keyboard_v1 *keyboard;
static struct xkb_context *context;
static struct xkb_keymap *keymap;
static struct xkb_state *state;
static struct xkb_compose_state *compose;
static uint32_t serial, purpose, hints, epoch;
static bool active, pending_active, available;
static bool awaiting_commit;
static uint32_t commit_time;
static char commands[128][64];
static unsigned command_head, command_count;
static const int keys[]={2,3,4,5,6,7,8,9,10,11,12,13,16,17,18,19,20,21,22,23,24,25,26,27,
    30,31,32,33,34,35,36,37,38,39,40,41,43,44,45,46,47,48,49,50,51,52,53,57};

static void string(const char *s) {
    putchar('"');
    for(const unsigned char *p=(const unsigned char *)s;*p;p++) {
        if(*p=='"'||*p=='\\') {putchar('\\');putchar(*p);}
        else if(*p<32) printf("\\u%04x",*p);
        else putchar(*p);
    }
    putchar('"');
}
static void error(const char *message) {fputs("{\"event\":\"error\",\"message\":",stdout);string(message);puts("}");}
static uint32_t now(void) {struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec*1000+t.tv_nsec/1000000;}
static xkb_mod_mask_t mask(unsigned flags) {
    const char *names[]={XKB_MOD_NAME_SHIFT,XKB_MOD_NAME_CTRL,XKB_MOD_NAME_ALT,XKB_MOD_NAME_LOGO,"Mod5"};
    xkb_mod_mask_t result=0;
    for(unsigned i=0;i<5;i++) if(flags&(1u<<i)) {
        xkb_mod_index_t index=xkb_keymap_mod_get_index(keymap,names[i]);
        if(index!=XKB_MOD_INVALID) result|=1u<<index;
    }
    return result;
}
static bool send_map(struct xkb_keymap *map) {
    if(!keyboard) return true;
    char *text=xkb_keymap_get_as_string(map,XKB_KEYMAP_FORMAT_TEXT_V1);
    if(!text) return false;
    size_t size=strlen(text)+1;
    int fd=memfd_create("waykeys-keymap",MFD_CLOEXEC);
    bool ok=fd>=0 && write(fd,text,size)==(ssize_t)size;
    if(ok) zwp_virtual_keyboard_v1_keymap(keyboard,WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1,fd,size);
    if(fd>=0) close(fd);
    free(text); return ok;
}
static void emit_layout(void) {
    fputs("{\"event\":\"layout\",\"keys\":{",stdout);
    for(unsigned i=0;i<sizeof(keys)/sizeof(*keys);i++) {
        printf("%s\"%d\":[",i?",":"",keys[i]);
        unsigned mods[]={0,1,16,17};
        for(unsigned j=0;j<4;j++) {
            xkb_state_update_mask(state,mask(mods[j]),0,0,0,0,0);
            char label[64]={0};xkb_state_key_get_utf8(state,keys[i]+8,label,sizeof(label));
            if(!label[0]) {
                xkb_keysym_t sym=xkb_state_key_get_one_sym(state,keys[i]+8);
                switch(sym) {
                    case XKB_KEY_dead_acute:strcpy(label,"´");break;
                    case XKB_KEY_dead_grave:strcpy(label,"`");break;
                    case XKB_KEY_dead_circumflex:strcpy(label,"^");break;
                    case XKB_KEY_dead_diaeresis:strcpy(label,"¨");break;
                    case XKB_KEY_dead_tilde:strcpy(label,"~");break;
                    default:break;
                }
            }
            if(j) putchar(',');
            string(label);
        }
        putchar(']');
    }
    fputs("},\"dead\":{",stdout);
    bool first=true;
    for(unsigned i=0;i<sizeof(keys)/sizeof(*keys);i++) {
        unsigned mods[]={0,1,16,17};
        char names[4][64]={{0}};bool dead=false;
        for(unsigned j=0;j<4;j++) {
            xkb_state_update_mask(state,mask(mods[j]),0,0,0,0,0);
            xkb_keysym_get_name(xkb_state_key_get_one_sym(state,keys[i]+8),names[j],sizeof(names[j]));
            if(!strncmp(names[j],"dead_",5))dead=true;
            else names[j][0]=0;
        }
        if(!dead)continue;
        printf("%s\"%d\":[",first?"":",",keys[i]);first=false;
        for(unsigned j=0;j<4;j++){if(j)putchar(',');string(names[j]);}
        putchar(']');
    }
    puts("}}");xkb_state_update_mask(state,0,0,0,0,0,0);
}
static bool layout(const char *name) {
    if(!*name||strlen(name)>39||strspn(name,"abcdefghijklmnopqrstuvwxyz0123456789_")!=strlen(name)) return false;
    struct xkb_rule_names rules={.layout=name};
    struct xkb_keymap *map=xkb_keymap_new_from_names(context,&rules,XKB_KEYMAP_COMPILE_NO_FLAGS);
    if(!map) return false;
    struct xkb_state *next=xkb_state_new(map);
    if(!next||!send_map(map)) {if(next)xkb_state_unref(next);xkb_keymap_unref(map);return false;}
    if(state)xkb_state_unref(state);
    if(keymap)xkb_keymap_unref(keymap);
    keymap=map;state=next;if(compose)xkb_compose_state_reset(compose);
    emit_layout();return true;
}
static void commit(const char *text) {
    zwp_input_method_v2_commit_string(im,text);zwp_input_method_v2_commit(im,serial);
    // Wait for the application's next state before committing another string.
    // WebKit (and other async text inputs) may discard a commit with a stale
    // serial if several virtual keys are delivered in the same event turn.
    awaiting_commit=true;commit_time=now();
}
static void raw_key(unsigned code,unsigned flags) {
    zwp_virtual_keyboard_v1_modifiers(keyboard,mask(flags),0,0,0);
    uint32_t time=now();
    zwp_virtual_keyboard_v1_key(keyboard,time,code,WL_KEYBOARD_KEY_STATE_PRESSED);
    zwp_virtual_keyboard_v1_key(keyboard,time,code,WL_KEYBOARD_KEY_STATE_RELEASED);
    zwp_virtual_keyboard_v1_modifiers(keyboard,0,0,0,0);
}
static void key(unsigned code,unsigned flags) {
    if(code>247||flags>31) return;
    xkb_state_update_mask(state,mask(flags),0,0,0,0,0);
    char text[128]={0};
    if(active&&available&&!(flags&(2|4|8))) {
        xkb_keysym_t sym=xkb_state_key_get_one_sym(state,code+8);
        if(compose) {
            if(code==14&&xkb_compose_state_get_status(compose)==XKB_COMPOSE_COMPOSING) {xkb_compose_state_reset(compose);return;}
            xkb_compose_state_feed(compose,sym);
            switch(xkb_compose_state_get_status(compose)) {
                case XKB_COMPOSE_COMPOSING:return;
                case XKB_COMPOSE_COMPOSED:xkb_compose_state_get_utf8(compose,text,sizeof(text));xkb_compose_state_reset(compose);break;
                case XKB_COMPOSE_CANCELLED:xkb_compose_state_reset(compose);return;
                default:xkb_state_key_get_utf8(state,code+8,text,sizeof(text));break;
            }
        } else xkb_state_key_get_utf8(state,code+8,text,sizeof(text));
        if(text[0]&&(unsigned char)text[0]>=32&&text[0]!=127) {commit(text);return;}
    }
    raw_key(code,flags);
}
static void text_key(unsigned scalar) {
    if(!scalar||scalar>0x10ffff||(scalar>=0xd800&&scalar<=0xdfff)) return;
    char text[8];xkb_keysym_to_utf8(xkb_utf32_to_keysym(scalar),text,sizeof(text));
    if(active&&available) {commit(text);return;}
    // A temporary Unicode mapping covers symbols absent from the active layout.
    char definition[512];
    snprintf(definition,sizeof(definition),"xkb_keymap { xkb_keycodes { include \"evdev+aliases(qwerty)\" }; xkb_types { include \"complete\" }; xkb_compatibility { include \"complete\" }; xkb_symbols { include \"pc+us\" key <I255> { [ U%04X ] }; }; };",scalar);
    struct xkb_keymap *map=xkb_keymap_new_from_string(context,definition,XKB_KEYMAP_FORMAT_TEXT_V1,0);
    if(map&&send_map(map)) {raw_key(247,0);send_map(keymap);}
    if(map)xkb_keymap_unref(map);
}
static void activate(void *d,struct zwp_input_method_v2 *m) {(void)d;(void)m;pending_active=true;purpose=hints=0;epoch++;if(compose)xkb_compose_state_reset(compose);}
static void deactivate(void *d,struct zwp_input_method_v2 *m) {(void)d;(void)m;pending_active=false;}
static void surrounding(void *d,struct zwp_input_method_v2 *m,const char *text,uint32_t cursor,uint32_t anchor) {(void)d;(void)m;(void)text;(void)cursor;(void)anchor;}
static void cause(void *d,struct zwp_input_method_v2 *m,uint32_t c) {(void)d;(void)m;(void)c;}
static void content(void *d,struct zwp_input_method_v2 *m,uint32_t h,uint32_t p) {(void)d;(void)m;hints=h;purpose=p;}
static void done(void *d,struct zwp_input_method_v2 *m) {(void)d;(void)m;serial++;active=pending_active;awaiting_commit=false;printf("{\"event\":\"context\",\"active\":%s,\"purpose\":%u,\"epoch\":%u}\n",active?"true":"false",purpose,epoch);}
static void unavailable(void *d,struct zwp_input_method_v2 *m) {(void)d;(void)m;available=false;active=false;puts("{\"event\":\"ime-unavailable\"}");}
static const struct zwp_input_method_v2_listener im_events={activate,deactivate,surrounding,cause,content,done,unavailable};
static void global(void *d,struct wl_registry *r,uint32_t id,const char *interface,uint32_t version) {
    (void)d;(void)version;
    if(!strcmp(interface,"wl_seat")&&!seat) seat=wl_registry_bind(r,id,&wl_seat_interface,1);
    else if(!strcmp(interface,"zwp_virtual_keyboard_manager_v1")) vk_manager=wl_registry_bind(r,id,&zwp_virtual_keyboard_manager_v1_interface,1);
    else if(!strcmp(interface,"zwp_input_method_manager_v2")) im_manager=wl_registry_bind(r,id,&zwp_input_method_manager_v2_interface,1);
}
static void removed(void *d,struct wl_registry *r,uint32_t id) {(void)d;(void)r;(void)id;}
static const struct wl_registry_listener registry_events={global,removed};
static void command(char *line) {
    char op[24]={0},arg[40]={0},extra;unsigned code,flags;
    if(sscanf(line,"key %u %u %c",&code,&flags,&extra)==2) key(code,flags);
    else if(sscanf(line,"text %u %c",&code,&extra)==1) text_key(code);
    else if(sscanf(line,"%23s %39s %c",op,arg,&extra)==2&&!strcmp(op,"layout")) {if(!layout(arg))error("That keyboard layout is unavailable.");}
}
int main(int argc,char **argv) {
    setlocale(LC_ALL,"");setvbuf(stdout,NULL,_IOLBF,0);
    context=xkb_context_new(XKB_CONTEXT_NO_FLAGS);if(!context)return 1;
    struct xkb_compose_table *table=xkb_compose_table_new_from_locale(context,setlocale(LC_CTYPE,NULL),0);
    if(table) {compose=xkb_compose_state_new(table,0);xkb_compose_table_unref(table);}
    if(argc==3&&!strcmp(argv[1],"--labels")) return layout(argv[2])?0:1;
    display=wl_display_connect(NULL);if(!display){error("Cannot connect to the Wayland display.");return 1;}
    struct wl_registry *registry=wl_display_get_registry(display);wl_registry_add_listener(registry,&registry_events,NULL);
    if(wl_display_roundtrip(display)<0||!seat||!vk_manager){error("This compositor does not support virtual keyboards.");return 1;}
    keyboard=zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(vk_manager,seat);
    if(!layout(argc>1?argv[1]:"us")){error("Could not load the keyboard layout.");return 1;}
    if(im_manager) {im=zwp_input_method_manager_v2_get_input_method(im_manager,seat);available=true;zwp_input_method_v2_add_listener(im,&im_events,NULL);}
    wl_display_roundtrip(display);
    printf("{\"event\":\"ready\",\"input_method\":%s}\n",available?"true":"false");
    char buffer[512];size_t used=0;
    for(;;) {
        // A nonconforming input must not wedge the keyboard permanently.
        if(awaiting_commit && now()-commit_time>=250)awaiting_commit=false;
        while(command_count&&!awaiting_commit) {
            command(commands[command_head]);command_head=(command_head+1)%128;command_count--;
        }
        while(wl_display_prepare_read(display)!=0) if(wl_display_dispatch_pending(display)<0)goto finish;
        wl_display_flush(display);
        struct pollfd fds[]={{wl_display_get_fd(display),POLLIN,0},{STDIN_FILENO,POLLIN,0}};
        int result=poll(fds,2,awaiting_commit?250:-1);
        if(result<0) {wl_display_cancel_read(display);if(errno==EINTR)continue;break;}
        if(fds[0].revents&POLLIN) {if(wl_display_read_events(display)<0)break;} else wl_display_cancel_read(display);
        if(wl_display_dispatch_pending(display)<0)break;
        if(fds[0].revents&(POLLERR|POLLHUP))break;
        if(fds[1].revents&POLLIN) {
            ssize_t n=read(STDIN_FILENO,buffer+used,sizeof(buffer)-used-1);if(n<=0)break;
            used+=n;buffer[used]=0;char *start=buffer,*end;
            while((end=strchr(start,'\n'))) {
                *end=0;
                if(strlen(start)>=sizeof(commands[0])||command_count==128){error("Keyboard input queue overflow.");goto finish;}
                strcpy(commands[(command_head+command_count)%128],start);command_count++;
                start=end+1;
            }
            used-=start-buffer;memmove(buffer,start,used);
            if(used>=sizeof(buffer)-1){error("Invalid input command.");break;}
        } else if(fds[1].revents&(POLLHUP|POLLERR))break;
    }
finish:
    zwp_virtual_keyboard_v1_modifiers(keyboard,0,0,0,0);
    if(im)zwp_input_method_v2_destroy(im);
    zwp_virtual_keyboard_v1_destroy(keyboard);wl_display_flush(display);wl_display_disconnect(display);
    if(compose)xkb_compose_state_unref(compose);
    xkb_state_unref(state);xkb_keymap_unref(keymap);xkb_context_unref(context);
    return 0;
}
