#!/usr/bin/python3
"""Private-bus fixture, launched only by session-check.py portal. No host input."""
from gi.repository import Gio, GLib
from pathlib import Path
import os, subprocess
root=Path(os.environ['FEATHER_PORTAL_TEST_ROOT'])
assert os.environ['XDG_CONFIG_HOME']==str(root/'config')
name='org.freedesktop.impl.portal.desktop.feather'
data=root/'data'
portals=data/'xdg-desktop-portal/portals';portals.mkdir(parents=True,exist_ok=True)
(portals/'feather.portal').write_text(f'[portal]\nDBusName={name}\nInterfaces=org.freedesktop.impl.portal.FileChooser;\nUseIn=FeatherDE;\n')
conf=root/'config/xdg-desktop-portal';conf.mkdir(parents=True,exist_ok=True)
(conf/'featherde-portals.conf').write_text('[preferred]\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=feather\n')
services=data/'dbus-1/services';services.mkdir(parents=True,exist_ok=True)
exe=Path('target/release/feather-de').resolve()
(services/f'{name}.service').write_text(f'[D-BUS Service]\nName={name}\nExec="{exe}" --file-portal\n')
# Activated backend must inherit this compositor's socket, never the host's.
subprocess.run(['dbus-update-activation-environment','WAYLAND_DISPLAY','XDG_CURRENT_DESKTOP','FEATHER_PICKER_TEST_FILE'],check=True)
portal=subprocess.Popen(['/usr/lib/xdg-desktop-portal','--verbose'])
try:
    bus=Gio.bus_get_sync(Gio.BusType.SESSION,None)
    replies={}; loop=GLib.MainLoop()
    def response(conn,sender,path,interface,signal,params):
        replies[path]=params.unpack();loop.quit()
    sub=bus.signal_subscribe('org.freedesktop.portal.Desktop','org.freedesktop.portal.Request','Response',None,None,Gio.DBusSignalFlags.NONE,response)
    def request(method,options,cancel=False):
        handle=bus.call_sync('org.freedesktop.portal.Desktop','/org/freedesktop/portal/desktop','org.freedesktop.portal.FileChooser',method,
            GLib.Variant('(ssa{sv})',('', 'Выбор через портал', options)),GLib.VariantType.new('(o)'),Gio.DBusCallFlags.NONE,9000,None).unpack()[0]
        if cancel:
            def close():
                bus.call_sync('org.freedesktop.portal.Desktop',handle,'org.freedesktop.portal.Request','Close',None,None,Gio.DBusCallFlags.NONE,2000,None)
                loop.quit();return False
            timer=GLib.timeout_add(400,close);loop.run()
            return
        expired=[False]
        def timeout():expired[0]=True;loop.quit();return False
        timer=GLib.timeout_add(7000,timeout)
        while handle not in replies and not expired[0]:loop.run()
        if not expired[0]:GLib.source_remove(timer)
        assert not expired[0],f'{method} timed out'
        status,result=replies.pop(handle);assert status==0,(status,result)
        return result
    folder=bytes(os.fsencode(root))+b'\0'
    filters=[('PNG',[(1,'image/png')])]
    result=request('OpenFile',{'current_folder':GLib.Variant('ay',folder),'multiple':GLib.Variant('b',True),
        'filters':GLib.Variant('a(sa(us))',filters),'choices':GLib.Variant('a(ssa(ss)s)',[('preview','Preview',[],'true')])})
    assert result['uris']==[(root/'выбор #1.png').as_uri()],result
    assert result['choices']==[('preview','true')],result
    assert result['current_filter']==('PNG',[(1,'image/png')]),result
    result=request('SaveFile',{'current_folder':GLib.Variant('ay',folder),'current_name':GLib.Variant('s','new file.png')})
    assert result['uris']==[(root/'new file.png').as_uri()],result
    assert not (root/'new file.png').exists(),'picker must not write user data'
    empty=root/'empty';empty.mkdir(exist_ok=True)
    request('OpenFile',{'directory':GLib.Variant('b',True),'current_folder':GLib.Variant('ay',os.fsencode(empty)+b'\0')},cancel=True)
    # Backend exports per-request objects only for the lifetime of the chooser.
    GLib.usleep(150000)
    children=[]
    for p in Path('/proc').iterdir():
        if p.name.isdigit():
            try:
                cmd=(p/'cmdline').read_bytes()
                if b'feather-files\0--file-picker' in cmd and (p/'environ').read_bytes().find(os.fsencode(str(root)))>=0:children.append(int(p.name))
            except (FileNotFoundError,PermissionError,ProcessLookupError):pass
    assert not children,f'chooser survived cancel: {children}'
    print('[feather-portal-check] frontend OpenFile filters choices SaveFile Close passed',flush=True)
finally:
    portal.terminate()
    try:portal.wait(timeout=2)
    except subprocess.TimeoutExpired:portal.kill();portal.wait()
