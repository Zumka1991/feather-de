"""A Tauri-like window: GTK3 without decorations, WebKitGTK page with its own
titlebar buttons that call minimize/close through a script message handler.
Logs every handled click to the file given as argv[1]."""
import os, sys, gi
gi.require_version('Gtk', '3.0')
gi.require_version('WebKit2', '4.1')
from gi.repository import Gtk, WebKit2, GLib

LOG = sys.argv[1] if len(sys.argv) > 1 else '/tmp/feather-tauri.log'
def log(s):
    with open(LOG, 'a') as f:
        f.write(s + '\n')

GLib.set_prgname('feather-tauri-test')
win = Gtk.Window(title='Tauri test')
win.set_decorated(False)
win.set_default_size(480, 300)
ucm = WebKit2.UserContentManager()
ucm.register_script_message_handler('feather')
def on_message(_m, result):
    cmd = result.get_js_value().to_string()
    log('click ' + cmd)
    if cmd == 'minimize':
        win.iconify()
        if os.environ.get('TAURI_PRESENT'):
            # Like a tray icon: the app shows itself again
            GLib.timeout_add(800, lambda: (log('present'), win.present(), False)[2])
    elif cmd == 'close':
        Gtk.main_quit()
ucm.connect('script-message-received::feather', on_message)
view = WebKit2.WebView.new_with_user_content_manager(ucm)
html = """<html><body style='margin:0;font:14px sans-serif'>
<div style='position:fixed;left:0;top:0;right:0;height:40px;background:#ddd'>
<button id=min style='position:fixed;left:10px;top:5px;width:60px;height:30px'
 onclick="webkit.messageHandlers.feather.postMessage('minimize')">_</button>
<button id=ping style='position:fixed;left:80px;top:5px;width:60px;height:30px'
 onclick="webkit.messageHandlers.feather.postMessage('ping')">ping</button>
<button id=close style='position:fixed;left:150px;top:5px;width:60px;height:30px'
 onclick="webkit.messageHandlers.feather.postMessage('close')">x</button>
</div></body></html>"""
view.load_html(html, None)
def loaded(_v, ev):
    if ev == WebKit2.LoadEvent.FINISHED:
        log('loaded')
view.connect('load-changed', loaded)
win.add(view)
win.connect('destroy', Gtk.main_quit)
win.show_all()
Gtk.main()
log('exited')
