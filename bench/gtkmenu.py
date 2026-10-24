import sys, gi
gi.require_version('Gtk', sys.argv[1] if len(sys.argv) > 1 else '3.0')
from gi.repository import Gtk, Gio, GLib
LOG = "/tmp/feather-gtkmenu.log"
def log(s):
    open(LOG, 'a').write(s + "\n")
class App(Gtk.Application):
    def __init__(self):
        super().__init__(application_id='org.feather.MenuTest')
    def do_startup(self):
        Gtk.Application.do_startup(self)
        a = Gio.SimpleAction.new('new', None); a.connect('activate', lambda *x: log('app.new')); self.add_action(a)
        q = Gio.SimpleAction.new('quit', None); q.connect('activate', lambda *x: (log('app.quit'), self.quit())); self.add_action(q)
        self.set_accels_for_action('app.new', ['<Primary>n'])
        menubar = Gio.Menu()
        file = Gio.Menu()
        sec = Gio.Menu(); sec.append('New', 'app.new'); sec.append('Save', 'win.save'); file.append_section(None, sec)
        sec2 = Gio.Menu(); sec2.append('Quit', 'app.quit'); file.append_section(None, sec2)
        menubar.append_submenu('File', file)
        view = Gio.Menu(); view.append('Sidebar', 'win.sidebar')
        menubar.append_submenu('View', view)
        self.set_menubar(menubar)
    def do_activate(self):
        w = Gtk.ApplicationWindow(application=self, title='Menu test')
        s = Gio.SimpleAction.new('save', None); s.set_enabled(False); w.add_action(s)
        t = Gio.SimpleAction.new_stateful('sidebar', None, GLib.Variant('b', True)); w.add_action(t)
        w.set_default_size(420, 240)
        if hasattr(w, 'show_all'): w.show_all()
        else: w.present()
        log('shown, shell shows menubar=%s' % Gtk.Settings.get_default().get_property('gtk-shell-shows-menubar'))
App().run([sys.argv[0]])
