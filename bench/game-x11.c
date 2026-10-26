// Owned X11 windows resembling Proton's game + anonymous helper, no host input.
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <xcb/xcb.h>
static xcb_connection_t *conn;static xcb_screen_t *screen;
static xcb_window_t window(const char *title) {
 xcb_window_t id=xcb_generate_id(conn);uint32_t values[]={0x207048,XCB_EVENT_MASK_EXPOSURE|XCB_EVENT_MASK_BUTTON_PRESS|XCB_EVENT_MASK_BUTTON_RELEASE|XCB_EVENT_MASK_STRUCTURE_NOTIFY};
 xcb_create_window(conn,XCB_COPY_FROM_PARENT,id,screen->root,0,0,640,360,0,XCB_WINDOW_CLASS_INPUT_OUTPUT,screen->root_visual,XCB_CW_BACK_PIXEL|XCB_CW_EVENT_MASK,values);
 const char cls[]="steam_app_default\0steam_app_default";
 xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,XCB_ATOM_WM_CLASS,XCB_ATOM_STRING,8,sizeof(cls),cls);
 xcb_change_property(conn,XCB_PROP_MODE_REPLACE,id,XCB_ATOM_WM_NAME,XCB_ATOM_STRING,8,strlen(title),title);
 xcb_map_window(conn,id);return id;
}
int main(void) {
 setbuf(stdout,NULL);int screen_index;conn=xcb_connect(NULL,&screen_index);assert(!xcb_connection_has_error(conn));
 xcb_screen_iterator_t it=xcb_setup_roots_iterator(xcb_get_setup(conn));for(int i=0;i<screen_index;i++)xcb_screen_next(&it);screen=it.data;
 window("");window("Lineage fixture");window("Other game fixture");xcb_flush(conn);
 xcb_generic_event_t *event;
 while((event=xcb_wait_for_event(conn))) {
  uint8_t type=event->response_type&~0x80;
  if(type==XCB_BUTTON_PRESS||type==XCB_BUTTON_RELEASE) {xcb_button_press_event_t *b=(xcb_button_press_event_t*)event;printf("button %u %u %d %d\n",b->detail,type==XCB_BUTTON_PRESS,b->event_x,b->event_y);}
  if(type==XCB_CONFIGURE_NOTIFY) {xcb_configure_notify_event_t *c=(xcb_configure_notify_event_t*)event;printf("size %u %u\n",c->width,c->height);}
  free(event);
 }
 return 0;
}
