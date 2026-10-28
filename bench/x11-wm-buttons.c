// Owned Steam-like X11 window with its own titlebar: asks the WM to maximize, restore
// (_NET_WM_STATE) and minimize (WM_CHANGE_STATE), which xwayland-satellite ignores. No host input.
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <xcb/xcb.h>
static xcb_connection_t *conn;static xcb_screen_t *screen;static xcb_window_t id;
static xcb_atom_t atom(const char *n){xcb_intern_atom_reply_t *r=xcb_intern_atom_reply(conn,xcb_intern_atom(conn,0,strlen(n),n),NULL);xcb_atom_t a=r->atom;free(r);return a;}
static void msg(const char *type,uint32_t d0,uint32_t d1,uint32_t d2){xcb_client_message_event_t m={0};m.response_type=XCB_CLIENT_MESSAGE;m.format=32;m.window=id;m.type=atom(type);m.data.data32[0]=d0;m.data.data32[1]=d1;m.data.data32[2]=d2;m.data.data32[3]=1;
 xcb_send_event(conn,0,screen->root,XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT|XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY,(const char*)&m);xcb_flush(conn);}
int main(void){setbuf(stdout,NULL);int si;conn=xcb_connect(NULL,&si);screen=xcb_setup_roots_iterator(xcb_get_setup(conn)).data;
 id=xcb_generate_id(conn);uint32_t v[]={0x305070,XCB_EVENT_MASK_STRUCTURE_NOTIFY|XCB_EVENT_MASK_PROPERTY_CHANGE};
 xcb_create_window(conn,XCB_COPY_FROM_PARENT,id,screen->root,0,0,800,600,0,XCB_WINDOW_CLASS_INPUT_OUTPUT,screen->root_visual,XCB_CW_BACK_PIXEL|XCB_CW_EVENT_MASK,v);
 const char cls[]="steamwebhelper\0steam";xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,XCB_ATOM_WM_CLASS,XCB_ATOM_STRING,8,sizeof(cls),cls);
 const char t[]="Steam";xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,XCB_ATOM_WM_NAME,XCB_ATOM_STRING,8,strlen(t),t);
 uint32_t motif[5]={2,0,0,0,0};xcb_atom_t mh=atom("_MOTIF_WM_HINTS");xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,mh,mh,32,5,motif);
 xcb_map_window(conn,id);xcb_flush(conn);
 for(int tick=0;;tick++){
  if(tick==150){printf("maximize\n");msg("_NET_WM_STATE",1,atom("_NET_WM_STATE_MAXIMIZED_VERT"),atom("_NET_WM_STATE_MAXIMIZED_HORZ"));}
  if(tick==300){printf("unmaximize\n");msg("_NET_WM_STATE",0,atom("_NET_WM_STATE_MAXIMIZED_VERT"),atom("_NET_WM_STATE_MAXIMIZED_HORZ"));}
  if(tick==450){printf("iconify\n");msg("WM_CHANGE_STATE",3,0,0);}
  xcb_generic_event_t *e=xcb_poll_for_event(conn);if(!e){if(xcb_connection_has_error(conn))return 0;usleep(10000);continue;}
  uint8_t ty=e->response_type&~0x80;
  if(ty==XCB_CONFIGURE_NOTIFY){xcb_configure_notify_event_t *c=(void*)e;if(c->window==id)printf("size %u %u\n",c->width,c->height);}
  if(ty==XCB_UNMAP_NOTIFY)printf("unmapped\n");
  free(e);}
}
