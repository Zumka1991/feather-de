// Private nested compositor fixture; never injects host input.
#define _GNU_SOURCE
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"
#include "pointer-constraints-unstable-v1-client-protocol.h"
#include "relative-pointer-unstable-v1-client-protocol.h"

static struct wl_display *display;
static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct wl_seat *seat;
static struct wl_pointer *pointer;
static struct wl_surface *surface;
static struct wl_buffer *buffer;
static struct xdg_wm_base *wm;
static struct xdg_surface *xdg;
static struct xdg_toplevel *top;
static struct zwp_pointer_constraints_v1 *constraints;
static struct zwp_relative_pointer_manager_v1 *relative_manager;
static struct zwp_locked_pointer_v1 *locked;
static struct zwp_confined_pointer_v1 *confined;
static int lock_active, mapped;
static unsigned relative_count, press_count, release_count;

static void ping(void *d, struct xdg_wm_base *w, uint32_t serial) { (void)d; xdg_wm_base_pong(w,serial); }
static const struct xdg_wm_base_listener wm_listener={.ping=ping};
static void configured(void *d, struct xdg_surface *s, uint32_t serial) {
    (void)d; xdg_surface_ack_configure(s,serial);
    if (!mapped) { wl_surface_attach(surface,buffer,0,0); wl_surface_damage(surface,0,0,640,360); wl_surface_commit(surface); mapped=1; }
}
static const struct xdg_surface_listener xdg_listener={.configure=configured};
static void top_configure(void *d,struct xdg_toplevel *t,int32_t w,int32_t h,struct wl_array *a) { (void)d;(void)t;(void)w;(void)h;(void)a; }
static void top_close(void *d,struct xdg_toplevel *t) { (void)d;(void)t; }
static const struct xdg_toplevel_listener top_listener={.configure=top_configure,.close=top_close};
static void is_locked(void *d,struct zwp_locked_pointer_v1 *p) {
    (void)d; lock_active=1; printf("locked\n");
    zwp_locked_pointer_v1_set_cursor_position_hint(p,wl_fixed_from_int(140),wl_fixed_from_int(80));
    wl_surface_commit(surface);
}
static void is_unlocked(void *d,struct zwp_locked_pointer_v1 *p) { (void)d;(void)p;lock_active=0;printf("unlocked\n"); }
static const struct zwp_locked_pointer_v1_listener lock_listener={.locked=is_locked,.unlocked=is_unlocked};
static void is_confined(void *d,struct zwp_confined_pointer_v1 *p) { (void)d;(void)p;printf("confined\n"); }
static void is_unconfined(void *d,struct zwp_confined_pointer_v1 *p) { (void)d;(void)p;printf("unconfined\n"); }
static const struct zwp_confined_pointer_v1_listener confine_listener={.confined=is_confined,.unconfined=is_unconfined};
static void relative(void *d,struct zwp_relative_pointer_v1 *p,uint32_t hi,uint32_t lo,wl_fixed_t dx,wl_fixed_t dy,wl_fixed_t ux,wl_fixed_t uy) {
    (void)d;(void)p;relative_count++;
    printf("relative %u %u %u %.3f %.3f %.3f %.3f\n",relative_count,hi,lo,wl_fixed_to_double(dx),wl_fixed_to_double(dy),wl_fixed_to_double(ux),wl_fixed_to_double(uy));
}
static const struct zwp_relative_pointer_v1_listener relative_listener={.relative_motion=relative};
static void enter(void *d,struct wl_pointer *p,uint32_t serial,struct wl_surface *s,wl_fixed_t x,wl_fixed_t y) { (void)d;(void)s;(void)x;(void)y;wl_pointer_set_cursor(p,serial,NULL,0,0);printf("enter\n"); xdg_toplevel_set_title(top,"Game metadata updated"); xdg_toplevel_set_app_id(top,"feather-game-fixture-updated"); }
static void leave(void *d,struct wl_pointer *p,uint32_t serial,struct wl_surface *s) { (void)d;(void)p;(void)serial;(void)s;printf("leave\n"); }
static void motion(void *d,struct wl_pointer *p,uint32_t t,wl_fixed_t x,wl_fixed_t y) { (void)d;(void)p;(void)t;assert(!lock_active && "absolute motion delivered during lock"); printf("motion %.3f %.3f\n",wl_fixed_to_double(x),wl_fixed_to_double(y)); }
static void button(void *d,struct wl_pointer *p,uint32_t serial,uint32_t t,uint32_t b,uint32_t state) {
    (void)d;(void)p;(void)serial;(void)t;
    if(state==WL_POINTER_BUTTON_STATE_PRESSED) press_count++; else release_count++;
    printf("button %u %u\n",b,state);
    if(b==0x111) {
        if(state==WL_POINTER_BUTTON_STATE_PRESSED) {
            assert(!locked); locked=zwp_pointer_constraints_v1_lock_pointer(constraints,surface,pointer,NULL,ZWP_POINTER_CONSTRAINTS_V1_LIFETIME_ONESHOT);
            zwp_locked_pointer_v1_add_listener(locked,&lock_listener,NULL);
        } else if(locked) { lock_active=0;zwp_locked_pointer_v1_destroy(locked);locked=NULL; }
    } else if(b==0x112) {
        if(state==WL_POINTER_BUTTON_STATE_PRESSED) {
            struct wl_region *r=wl_compositor_create_region(compositor);wl_region_add(r,100,80,200,160);
            confined=zwp_pointer_constraints_v1_confine_pointer(constraints,surface,pointer,r,ZWP_POINTER_CONSTRAINTS_V1_LIFETIME_ONESHOT);
            zwp_confined_pointer_v1_add_listener(confined,&confine_listener,NULL);wl_region_destroy(r);
        } else if(confined) { zwp_confined_pointer_v1_destroy(confined);confined=NULL; }
    }
    wl_surface_commit(surface);
}
static void axis(void *d,struct wl_pointer *p,uint32_t t,uint32_t a,wl_fixed_t v) { (void)d;(void)p;(void)t;(void)a;(void)v; }
static void frame(void *d,struct wl_pointer *p) { (void)d;(void)p; }
static void source(void *d,struct wl_pointer *p,uint32_t s) { (void)d;(void)p;(void)s; }
static void stop(void *d,struct wl_pointer *p,uint32_t t,uint32_t a) { (void)d;(void)p;(void)t;(void)a; }
static void discrete(void *d,struct wl_pointer *p,uint32_t a,int32_t v) { (void)d;(void)p;(void)a;(void)v; }
static const struct wl_pointer_listener pointer_listener={.enter=enter,.leave=leave,.motion=motion,.button=button,.axis=axis,.frame=frame,.axis_source=source,.axis_stop=stop,.axis_discrete=discrete};
static void capabilities(void *d,struct wl_seat *s,uint32_t caps) {
    (void)d;if((caps&WL_SEAT_CAPABILITY_POINTER)&&!pointer) {
        pointer=wl_seat_get_pointer(s);wl_pointer_add_listener(pointer,&pointer_listener,NULL);
        struct zwp_relative_pointer_v1 *r=zwp_relative_pointer_manager_v1_get_relative_pointer(relative_manager,pointer);
        zwp_relative_pointer_v1_add_listener(r,&relative_listener,NULL);
    }
}
static void seat_name(void *d,struct wl_seat *s,const char *n) { (void)d;(void)s;(void)n; }
static const struct wl_seat_listener seat_listener={.capabilities=capabilities,.name=seat_name};
static void global(void *d,struct wl_registry *r,uint32_t n,const char *iface,uint32_t v) {
    (void)d;(void)v;
    if(!strcmp(iface,"wl_compositor")) compositor=wl_registry_bind(r,n,&wl_compositor_interface,4);
    else if(!strcmp(iface,"wl_shm")) shm=wl_registry_bind(r,n,&wl_shm_interface,1);
    else if(!strcmp(iface,"wl_seat")) seat=wl_registry_bind(r,n,&wl_seat_interface,5);
    else if(!strcmp(iface,"xdg_wm_base")) { wm=wl_registry_bind(r,n,&xdg_wm_base_interface,1);xdg_wm_base_add_listener(wm,&wm_listener,NULL); }
    else if(!strcmp(iface,"zwp_pointer_constraints_v1")) constraints=wl_registry_bind(r,n,&zwp_pointer_constraints_v1_interface,1);
    else if(!strcmp(iface,"zwp_relative_pointer_manager_v1")) relative_manager=wl_registry_bind(r,n,&zwp_relative_pointer_manager_v1_interface,1);
}
static void removed(void *d,struct wl_registry *r,uint32_t n) { (void)d;(void)r;(void)n; }
static const struct wl_registry_listener registry_listener={.global=global,.global_remove=removed};
int main(void) {
    setbuf(stdout,NULL);display=wl_display_connect(NULL);assert(display);
    struct wl_registry *r=wl_display_get_registry(display);wl_registry_add_listener(r,&registry_listener,NULL);wl_display_roundtrip(display);
    assert(compositor&&shm&&wm&&seat&&constraints&&relative_manager);wl_seat_add_listener(seat,&seat_listener,NULL);
    int fd=memfd_create("feather-game-fixture",MFD_CLOEXEC);assert(fd>=0);int len=640*360*4;assert(!ftruncate(fd,len));
    unsigned *pixels=mmap(NULL,len,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);assert(pixels!=MAP_FAILED);for(int i=0;i<640*360;i++) pixels[i]=0xff207048;
    struct wl_shm_pool *pool=wl_shm_create_pool(shm,fd,len);buffer=wl_shm_pool_create_buffer(pool,0,640,360,640*4,WL_SHM_FORMAT_XRGB8888);wl_shm_pool_destroy(pool);close(fd);
    surface=wl_compositor_create_surface(compositor);xdg=xdg_wm_base_get_xdg_surface(wm,surface);xdg_surface_add_listener(xdg,&xdg_listener,NULL);
    top=xdg_surface_get_toplevel(xdg);xdg_toplevel_add_listener(top,&top_listener,NULL);xdg_toplevel_set_title(top,"Game pointer fixture");xdg_toplevel_set_app_id(top,"feather-game-fixture");wl_surface_commit(surface);
    while(wl_display_dispatch(display)>=0) {} return 0;
}
