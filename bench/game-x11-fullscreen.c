// Owned X11 window resembling a Proton game in borderless fullscreen: as big as the X screen,
// undecorated, _NET_WM_STATE_FULLSCREEN set before mapping (what winex11 does). No host input.
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <xcb/xcb.h>
static xcb_connection_t *conn;
static xcb_atom_t atom(const char *name) {
 xcb_intern_atom_reply_t *r=xcb_intern_atom_reply(conn,xcb_intern_atom(conn,0,strlen(name),name),NULL);assert(r);xcb_atom_t a=r->atom;free(r);return a;
}
int main(void) {
 setbuf(stdout,NULL);int screen_index;conn=xcb_connect(NULL,&screen_index);assert(!xcb_connection_has_error(conn));
 xcb_screen_iterator_t it=xcb_setup_roots_iterator(xcb_get_setup(conn));for(int i=0;i<screen_index;i++)xcb_screen_next(&it);xcb_screen_t *screen=it.data;
 printf("root %u %u\n",screen->width_in_pixels,screen->height_in_pixels);
 xcb_window_t id=xcb_generate_id(conn);uint32_t values[]={0x205080,XCB_EVENT_MASK_EXPOSURE|XCB_EVENT_MASK_BUTTON_PRESS|XCB_EVENT_MASK_STRUCTURE_NOTIFY};
 // FIXTURE_ENTER_FULLSCREEN: map as a normal 960x540 window, request fullscreen after 1 s
 int enter=getenv("FIXTURE_ENTER_FULLSCREEN")!=NULL;
 xcb_create_window(conn,XCB_COPY_FROM_PARENT,id,screen->root,0,0,enter?960:screen->width_in_pixels,enter?540:screen->height_in_pixels,0,XCB_WINDOW_CLASS_INPUT_OUTPUT,screen->root_visual,XCB_CW_BACK_PIXEL|XCB_CW_EVENT_MASK,values);
 const char cls[]="steam_app_238960\0steam_app_238960";
 xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,XCB_ATOM_WM_CLASS,XCB_ATOM_STRING,8,sizeof(cls),cls);
 const char title[]="Fullscreen game fixture";
 xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,XCB_ATOM_WM_NAME,XCB_ATOM_STRING,8,strlen(title),title);
 uint32_t motif[5]={2,0,0,0,0};xcb_atom_t mh=atom("_MOTIF_WM_HINTS");
 xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,mh,mh,32,5,motif);
 if(!enter&&!getenv("FIXTURE_NO_FULLSCREEN_STATE")){xcb_atom_t fs=atom("_NET_WM_STATE_FULLSCREEN");xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,atom("_NET_WM_STATE"),XCB_ATOM_ATOM,32,1,&fs);}
 xcb_map_window(conn,id);xcb_flush(conn);
 // FIXTURE_LEAVE_FULLSCREEN: after 3 s ask the window manager to drop fullscreen
 int leave=getenv("FIXTURE_LEAVE_FULLSCREEN")!=NULL;
 for(int tick=0;;tick++) {
  if(enter&&tick==100){xcb_client_message_event_t m={0};m.response_type=XCB_CLIENT_MESSAGE;m.format=32;m.window=id;m.type=atom("_NET_WM_STATE");m.data.data32[0]=1;m.data.data32[1]=atom("_NET_WM_STATE_FULLSCREEN");m.data.data32[3]=1;
   xcb_send_event(conn,0,screen->root,XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT|XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY,(const char*)&m);xcb_flush(conn);printf("enter fullscreen\n");}
  if(leave&&tick==300){xcb_client_message_event_t m={0};m.response_type=XCB_CLIENT_MESSAGE;m.format=32;m.window=id;m.type=atom("_NET_WM_STATE");m.data.data32[0]=0;m.data.data32[1]=atom("_NET_WM_STATE_FULLSCREEN");m.data.data32[3]=1;
   xcb_send_event(conn,0,screen->root,XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT|XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY,(const char*)&m);xcb_flush(conn);printf("leave fullscreen\n");}
  xcb_generic_event_t *event=xcb_poll_for_event(conn);
  if(!event){if(xcb_connection_has_error(conn))return 0;usleep(10000);continue;}
  uint8_t type=event->response_type&~0x80;
  if(type==XCB_CONFIGURE_NOTIFY) {xcb_configure_notify_event_t *c=(xcb_configure_notify_event_t*)event;if(c->window==id)printf("size %u %u at %d %d\n",c->width,c->height,c->x,c->y);}
  if(type==XCB_BUTTON_PRESS) {xcb_button_press_event_t *b=(xcb_button_press_event_t*)event;printf("button %u %d %d\n",b->detail,b->event_x,b->event_y);}
  free(event);
 }
 return 0;
}
