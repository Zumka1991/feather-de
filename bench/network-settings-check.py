#!/usr/bin/env python3
"""Owned nested compositor + private D-Bus mock NetworkManager.

Never touches the system D-Bus or real NetworkManager: DBUS_SYSTEM_BUS_ADDRESS is
pointed at the private dbus-run-session bus. Exercises Control Center -> Network
Settings navigation, saved Wi-Fi autoconnect + forget (preserving secrets),
Ethernet disconnect/connect, VPN/WireGuard connect/disconnect, WPA2 password sheet,
error persistence across background snapshots, RssAnon <= 60 MB and 0 idle CPU ticks/5 s.
"""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
from collections import Counter

if len(sys.argv) > 1 and sys.argv[1] == '--mock-nm':
    from gi.repository import Gio, GLib

    log_path = Path(sys.argv[2])
    ready_path = Path(sys.argv[3])

    def record(entry):
        with log_path.open('a') as f:
            f.write(json.dumps(entry) + '\n')

    state = {
        'wifi_enabled': True,
        'active_ap': '/test/ap/home',
        'eth_state': 100,
        'corp_fail_once': True,
        'active_vpns': {'/test/active/wg': '/test/vpn/wg'},
        'profiles': {
            '/test/saved/home': {
                'id': 'Feather 5G',
                'type': '802-11-wireless',
                'ssid': b'Feather 5G',
                'autoconnect': True,
                'key_mgmt': 'sae',
                'psk': 'home-secret-psk',
            },
            '/test/saved/cafe': {
                'id': 'CoffeeShop',
                'type': '802-11-wireless',
                'ssid': b'CoffeeShop',
                'autoconnect': True,
                'key_mgmt': 'wpa-psk',
                'psk': 'cafe-secret-psk',
            },
            '/test/eth_profile': {
                'id': 'Проводное подключение 1',
                'type': '802-3-ethernet',
                'autoconnect': True,
            },
            '/test/vpn/wg': {
                'id': 'Work WireGuard',
                'type': 'wireguard',
                'autoconnect': False,
            },
            '/test/vpn/corp': {
                'id': 'Corp VPN',
                'type': 'vpn',
                'autoconnect': False,
            },
        },
    }

    aps = {
        '/test/ap/home': {
            'Ssid': GLib.Variant('ay', b'Feather 5G'),
            'Strength': GLib.Variant('y', 92),
            'Flags': GLib.Variant('u', 1),
            'WpaFlags': GLib.Variant('u', 0),
            'RsnFlags': GLib.Variant('u', 0x400),
        },
        '/test/ap/neighbor': {
            'Ssid': GLib.Variant('ay', b'Neighbor WPA2'),
            'Strength': GLib.Variant('y', 74),
            'Flags': GLib.Variant('u', 1),
            'WpaFlags': GLib.Variant('u', 0),
            'RsnFlags': GLib.Variant('u', 0x100),
        },
    }

    def profile_settings(p):
        out = {
            'connection': {
                'id': GLib.Variant('s', p['id']),
                'type': GLib.Variant('s', p['type']),
                'autoconnect': GLib.Variant('b', p.get('autoconnect', True)),
            }
        }
        if p['type'] == '802-11-wireless':
            out['802-11-wireless'] = {'ssid': GLib.Variant('ay', p['ssid'])}
            if p.get('key_mgmt'):
                out['802-11-wireless-security'] = {
                    'key-mgmt': GLib.Variant('s', p['key_mgmt'])
                }
        return out

    def emit_props(conn, path, iface, changed):
        conn.emit_signal(
            None,
            path,
            'org.freedesktop.DBus.Properties',
            'PropertiesChanged',
            GLib.Variant('(sa{sv}as)', (iface, changed, [])),
        )

    NM_XML = """
    <node>
      <interface name="org.freedesktop.NetworkManager">
        <method name="GetDevices"><arg type="ao" direction="out"/></method>
        <method name="ActivateConnection">
          <arg type="o" direction="in"/><arg type="o" direction="in"/><arg type="o" direction="in"/>
          <arg type="o" direction="out"/>
        </method>
        <method name="DeactivateConnection"><arg type="o" direction="in"/></method>
        <method name="AddAndActivateConnection">
          <arg type="a{sa{sv}}" direction="in"/><arg type="o" direction="in"/><arg type="o" direction="in"/>
          <arg type="o" direction="out"/><arg type="o" direction="out"/>
        </method>
        <property name="WirelessEnabled" type="b" access="readwrite"/>
        <property name="WirelessHardwareEnabled" type="b" access="read"/>
        <property name="Devices" type="ao" access="read"/>
        <property name="ActiveConnections" type="ao" access="read"/>
      </interface>
    </node>"""

    SETTINGS_XML = """
    <node>
      <interface name="org.freedesktop.NetworkManager.Settings">
        <method name="ListConnections"><arg type="ao" direction="out"/></method>
        <property name="Connections" type="ao" access="read"/>
        <signal name="NewConnection"><arg type="o"/></signal>
        <signal name="ConnectionRemoved"><arg type="o"/></signal>
      </interface>
    </node>"""

    DEV_WIFI_XML = """
    <node>
      <interface name="org.freedesktop.NetworkManager.Device">
        <method name="Disconnect"/>
        <property name="DeviceType" type="u" access="read"/>
        <property name="State" type="u" access="read"/>
        <property name="Interface" type="s" access="read"/>
        <property name="ActiveConnection" type="o" access="read"/>
        <property name="AvailableConnections" type="ao" access="read"/>
      </interface>
      <interface name="org.freedesktop.NetworkManager.Device.Wireless">
        <method name="RequestScan"><arg type="a{sv}" direction="in"/></method>
        <property name="ActiveAccessPoint" type="o" access="read"/>
        <property name="AccessPoints" type="ao" access="read"/>
      </interface>
    </node>"""

    DEV_ETH_XML = """
    <node>
      <interface name="org.freedesktop.NetworkManager.Device">
        <method name="Disconnect"/>
        <property name="DeviceType" type="u" access="read"/>
        <property name="State" type="u" access="read"/>
        <property name="Interface" type="s" access="read"/>
        <property name="ActiveConnection" type="o" access="read"/>
        <property name="AvailableConnections" type="ao" access="read"/>
      </interface>
      <interface name="org.freedesktop.NetworkManager.Device.Wired">
        <property name="Carrier" type="b" access="read"/>
        <property name="Speed" type="u" access="read"/>
      </interface>
    </node>"""

    AP_XML = """
    <node>
      <interface name="org.freedesktop.NetworkManager.AccessPoint">
        <property name="Ssid" type="ay" access="read"/>
        <property name="Strength" type="y" access="read"/>
        <property name="Flags" type="u" access="read"/>
        <property name="WpaFlags" type="u" access="read"/>
        <property name="RsnFlags" type="u" access="read"/>
      </interface>
    </node>"""

    ACTIVE_XML = """
    <node>
      <interface name="org.freedesktop.NetworkManager.Connection.Active">
        <property name="Connection" type="o" access="read"/>
        <property name="State" type="u" access="read"/>
        <property name="VpnState" type="u" access="read"/>
      </interface>
    </node>"""

    PROFILE_XML = """
    <node>
      <interface name="org.freedesktop.NetworkManager.Settings.Connection">
        <method name="GetSettings"><arg type="a{sa{sv}}" direction="out"/></method>
        <method name="GetSecrets"><arg type="s" direction="in"/><arg type="a{sa{sv}}" direction="out"/></method>
        <method name="Update"><arg type="a{sa{sv}}" direction="in"/></method>
        <method name="Delete"/>
        <signal name="Updated"/>
        <signal name="Removed"/>
      </interface>
    </node>"""

    conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    registrations = {}

    def get_props_for(path, iface):
        if path == '/org/freedesktop/NetworkManager' and iface == 'org.freedesktop.NetworkManager':
            return {
                'WirelessEnabled': GLib.Variant('b', state['wifi_enabled']),
                'WirelessHardwareEnabled': GLib.Variant('b', True),
                'Devices': GLib.Variant('ao', ['/test/wlan0', '/test/eth0']),
                'ActiveConnections': GLib.Variant('ao', list(state['active_vpns'].keys())),
            }
        if path == '/org/freedesktop/NetworkManager/Settings' and iface == 'org.freedesktop.NetworkManager.Settings':
            return {'Connections': GLib.Variant('ao', list(state['profiles'].keys()))}
        if path == '/test/wlan0' and iface == 'org.freedesktop.NetworkManager.Device':
            wifi_profiles = [k for k, v in state['profiles'].items() if v['type'] == '802-11-wireless']
            return {
                'DeviceType': GLib.Variant('u', 2),
                'State': GLib.Variant('u', 100),
                'Interface': GLib.Variant('s', 'wlan0'),
                'ActiveConnection': GLib.Variant('o', '/test/active/wifi'),
                'AvailableConnections': GLib.Variant('ao', wifi_profiles),
            }
        if path == '/test/wlan0' and iface == 'org.freedesktop.NetworkManager.Device.Wireless':
            return {
                'ActiveAccessPoint': GLib.Variant('o', state['active_ap']),
                'AccessPoints': GLib.Variant('ao', list(aps.keys())),
            }
        if path == '/test/eth0' and iface == 'org.freedesktop.NetworkManager.Device':
            return {
                'DeviceType': GLib.Variant('u', 1),
                'State': GLib.Variant('u', state['eth_state']),
                'Interface': GLib.Variant('s', 'enp3s0'),
                'ActiveConnection': GLib.Variant('o', '/test/active/eth' if state['eth_state'] == 100 else '/'),
                'AvailableConnections': GLib.Variant('ao', ['/test/eth_profile']),
            }
        if path == '/test/eth0' and iface == 'org.freedesktop.NetworkManager.Device.Wired':
            return {
                'Carrier': GLib.Variant('b', True),
                'Speed': GLib.Variant('u', 1000),
            }
        if path in aps and iface == 'org.freedesktop.NetworkManager.AccessPoint':
            return aps[path]
        if path in state['active_vpns'] and iface == 'org.freedesktop.NetworkManager.Connection.Active':
            return {
                'Connection': GLib.Variant('o', state['active_vpns'][path]),
                'State': GLib.Variant('u', 2),
                'VpnState': GLib.Variant('u', 5),
            }
        return {}

    def on_get_prop(c, sender, path, iface, prop):
        return get_props_for(path, iface).get(prop)

    def on_set_prop(c, sender, path, iface, prop, value):
        if path == '/org/freedesktop/NetworkManager' and prop == 'WirelessEnabled':
            state['wifi_enabled'] = bool(value.unpack())
            record({'op': 'set_wifi', 'enabled': state['wifi_enabled']})
            emit_props(c, path, iface, {'WirelessEnabled': GLib.Variant('b', state['wifi_enabled'])})
            return True
        return False

    def register_profile(path):
        info = Gio.DBusNodeInfo.new_for_xml(PROFILE_XML).interfaces[0]
        reg_id = conn.register_object(path, info, on_method, on_get_prop, on_set_prop)
        registrations[path] = reg_id

    def register_active(path):
        if path in registrations:
            return
        info = Gio.DBusNodeInfo.new_for_xml(ACTIVE_XML).interfaces[0]
        registrations[path] = conn.register_object(path, info, on_method, on_get_prop, on_set_prop)

    def on_method(c, sender, path, iface, method, params, invocation):
        args = params.unpack()
        if path == '/org/freedesktop/NetworkManager':
            if method == 'GetDevices':
                invocation.return_value(GLib.Variant('(ao)', (['/test/wlan0', '/test/eth0'],)))
                return
            if method == 'ActivateConnection':
                profile, device, spec = args
                record({'op': 'activate', 'profile': profile, 'device': device, 'spec': spec})
                if device == '/test/eth0' or profile == '/test/eth_profile':
                    state['eth_state'] = 100
                    emit_props(c, '/test/eth0', 'org.freedesktop.NetworkManager.Device', {'State': GLib.Variant('u', 100)})
                    invocation.return_value(GLib.Variant('(o)', ('/test/active/eth',)))
                    return
                if profile in ('/test/vpn/wg', '/test/vpn/corp'):
                    act = '/test/active/wg' if profile.endswith('/wg') else '/test/active/corp'
                    state['active_vpns'][act] = profile
                    register_active(act)
                    emit_props(c, path, iface, {'ActiveConnections': GLib.Variant('ao', list(state['active_vpns'].keys()))})
                    invocation.return_value(GLib.Variant('(o)', (act,)))
                    return
                if device == '/test/wlan0':
                    state['active_ap'] = spec if spec != '/' else '/test/ap/home'
                    emit_props(c, '/test/wlan0', 'org.freedesktop.NetworkManager.Device.Wireless', {'ActiveAccessPoint': GLib.Variant('o', state['active_ap'])})
                    invocation.return_value(GLib.Variant('(o)', ('/test/active/wifi',)))
                    return
            if method == 'DeactivateConnection':
                (act,) = args
                record({'op': 'deactivate', 'active': act})
                if act == '/test/active/corp' and state['corp_fail_once']:
                    state['corp_fail_once'] = False
                    # Trigger a background signal right after the error so snapshot survival is tested.
                    def bump():
                        emit_props(c, '/org/freedesktop/NetworkManager', 'org.freedesktop.NetworkManager', {
                            'ActiveConnections': GLib.Variant('ao', list(state['active_vpns'].keys()))
                        })
                        return False
                    GLib.timeout_add(40, bump)
                    invocation.return_dbus_error('org.freedesktop.NetworkManager.Error.Failed', 'Шлюз VPN временно недоступен')
                    return
                state['active_vpns'].pop(act, None)
                emit_props(c, path, iface, {'ActiveConnections': GLib.Variant('ao', list(state['active_vpns'].keys()))})
                invocation.return_value(None)
                return
            if method == 'AddAndActivateConnection':
                settings, device, spec = args
                ssid = bytes(settings['802-11-wireless']['ssid'])
                psk = settings.get('802-11-wireless-security', {}).get('psk', '')
                record({'op': 'add_and_activate', 'ssid': ssid.decode('utf-8', 'replace'), 'psk_len': len(psk), 'device': device, 'spec': spec})
                new_path = '/test/saved/neighbor'
                state['profiles'][new_path] = {
                    'id': ssid.decode('utf-8', 'replace'),
                    'type': '802-11-wireless',
                    'ssid': ssid,
                    'autoconnect': True,
                    'key_mgmt': 'wpa-psk',
                    'psk': psk,
                }
                register_profile(new_path)
                state['active_ap'] = spec
                c.emit_signal(None, '/org/freedesktop/NetworkManager/Settings', 'org.freedesktop.NetworkManager.Settings', 'NewConnection', GLib.Variant('(o)', (new_path,)))
                emit_props(c, '/test/wlan0', 'org.freedesktop.NetworkManager.Device.Wireless', {'ActiveAccessPoint': GLib.Variant('o', state['active_ap'])})
                invocation.return_value(GLib.Variant('(oo)', (new_path, '/test/active/wifi')))
                return
        if path == '/org/freedesktop/NetworkManager/Settings' and method == 'ListConnections':
            invocation.return_value(GLib.Variant('(ao)', (list(state['profiles'].keys()),)))
            return
        if path == '/test/eth0' and method == 'Disconnect':
            record({'op': 'disconnect_eth', 'device': path})
            state['eth_state'] = 30
            emit_props(c, '/test/eth0', 'org.freedesktop.NetworkManager.Device', {'State': GLib.Variant('u', 30)})
            invocation.return_value(None)
            return
        if path == '/test/wlan0' and method == 'RequestScan':
            record({'op': 'scan'})
            invocation.return_value(None)
            return
        if path in state['profiles'] and iface == 'org.freedesktop.NetworkManager.Settings.Connection':
            p = state['profiles'][path]
            if method == 'GetSettings':
                invocation.return_value(GLib.Variant('(a{sa{sv}})', (profile_settings(p),)))
                return
            if method == 'GetSecrets':
                (sec_name,) = args
                sec = {'802-11-wireless-security': {'psk': GLib.Variant('s', p.get('psk', ''))}} if sec_name == '802-11-wireless-security' else {}
                invocation.return_value(GLib.Variant('(a{sa{sv}})', (sec,)))
                return
            if method == 'Update':
                (new_settings,) = args
                auto = bool(new_settings.get('connection', {}).get('autoconnect', True))
                psk = new_settings.get('802-11-wireless-security', {}).get('psk', '')
                p['autoconnect'] = auto
                record({'op': 'update_profile', 'path': path, 'autoconnect': auto, 'preserved_psk': psk == p.get('psk', '')})
                c.emit_signal(None, path, iface, 'Updated', None)
                invocation.return_value(None)
                return
            if method == 'Delete':
                record({'op': 'delete_profile', 'path': path})
                state['profiles'].pop(path, None)
                c.emit_signal(None, path, iface, 'Removed', None)
                c.emit_signal(None, '/org/freedesktop/NetworkManager/Settings', 'org.freedesktop.NetworkManager.Settings', 'ConnectionRemoved', GLib.Variant('(o)', (path,)))
                invocation.return_value(None)
                return
        invocation.return_dbus_error('org.freedesktop.DBus.Error.UnknownMethod', method)

    for xml, obj_path in [
        (NM_XML, '/org/freedesktop/NetworkManager'),
        (SETTINGS_XML, '/org/freedesktop/NetworkManager/Settings'),
        (DEV_WIFI_XML, '/test/wlan0'),
        (DEV_ETH_XML, '/test/eth0'),
        (AP_XML, '/test/ap/home'),
        (AP_XML, '/test/ap/neighbor'),
    ]:
        for iface_info in Gio.DBusNodeInfo.new_for_xml(xml).interfaces:
            conn.register_object(obj_path, iface_info, on_method, on_get_prop, on_set_prop)

    for prof_path in list(state['profiles'].keys()):
        register_profile(prof_path)
    for act_path in list(state['active_vpns'].keys()):
        register_active(act_path)

    loop = GLib.MainLoop()
    Gio.bus_own_name_on_connection(
        conn,
        'org.freedesktop.NetworkManager',
        Gio.BusNameOwnerFlags.NONE,
        lambda *_: ready_path.write_text('ready\n'),
        lambda *_: loop.quit(),
    )
    loop.run()
    sys.exit(0)

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--host', required=True)
p.add_argument('--scale', default='2')
p.add_argument('--output', type=Path)
p.add_argument('--screenshot', action='store_true')
p.add_argument('--duration', type=float, default=14.0)
p.add_argument('--protocol-summary', action='store_true')
a = p.parse_args()
if a.duration < 12.5:
    p.error('--duration must be at least 12.5 seconds')

root = (a.output or Path(tempfile.mkdtemp(prefix='feather-network-'))).resolve()
root.mkdir(parents=True, exist_ok=True)
calls_file = root / 'calls.jsonl'
calls_file.write_text('')
ready_file = root / 'nm.ready'
ready_file.unlink(missing_ok=True)
(root / 'pid').unlink(missing_ok=True)

env = {k: v for k, v in os.environ.items() if not k.startswith('FEATHER_DEBUG_')}
env.pop('WAYLAND_DEBUG', None)
env.update(
    WAYLAND_DISPLAY=a.host,
    PULSE_SERVER='unix:' + str(root / 'never-real-pulse'),
    XDG_CONFIG_HOME=str(root / 'config'),
    XDG_DATA_HOME=str(root / 'data'),
    FEATHER_SCALE=a.scale,
    FEATHER_XWAYLAND='0',
    FEATHER_PROFILE='1',
    FEATHER_PROFILE_UI='1',
    FEATHER_DEBUG_INPUT_ONLY='1',
    FEATHER_DEBUG_NETWORK_SETTINGS='1',
    RUST_LOG='feather_de=debug,smithay=warn',
)
if a.protocol_summary:
    env['WAYLAND_DEBUG'] = 'client'
if a.screenshot:
    env.update(
        FEATHER_DEBUG_SCREENSHOT=str(root / 'frame.png'),
        FEATHER_DEBUG_SCREENSHOT_MS='7100',
    )

shell_cmd = (
    'export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"; '
    'python3 "$1" --mock-nm "$2" "$3" & '
    'nm_pid=$!; '
    'while [ ! -f "$3" ]; do sleep 0.02; done; '
    'echo $$ > "$4"; '
    'trap "kill $nm_pid 2>/dev/null || true" EXIT; '
    'exec "$5" --windowed'
)
command = [
    'dbus-run-session',
    '--',
    'sh',
    '-c',
    shell_cmd,
    'network-settings-check',
    str(Path(__file__).resolve()),
    str(calls_file),
    str(ready_file),
    str(root / 'pid'),
    str(Path('target/release/feather-de').resolve()),
]

def read_sample(pid):
    proc = Path(f'/proc/{pid}')
    stat = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
    status = (proc / 'status').read_text().splitlines()
    return {
        'ticks': int(stat[11]) + int(stat[12]),
        'anon_kib': next(int(l.split()[1]) for l in status if l.startswith('RssAnon:')),
    }

with (root / 'compositor.log').open('w') as log:
    protocol = Counter()
    protocol_lock = threading.Lock()
    protocol_samples = []
    process = subprocess.Popen(
        command,
        env=env,
        stdout=log,
        stderr=subprocess.PIPE if a.protocol_summary else subprocess.STDOUT,
        start_new_session=True,
    )

    def collect_stderr():
        for data in iter(process.stderr.readline, b''):
            line = data.decode('utf-8', errors='replace')
            match = re.search(r'\b([a-z][a-z0-9_]+)(?:@|#)\d+\.([A-Za-z0-9_]+)\(', line)
            if match:
                with protocol_lock:
                    protocol['.'.join(match.groups())] += 1
            elif '[feather-' in line or 'panicked' in line or 'assertion' in line:
                log.write(line)
                log.flush()

    reader = threading.Thread(target=collect_stderr, daemon=True) if a.protocol_summary else None
    if reader:
        reader.start()

    start = time.monotonic()
    samples = []
    try:
        while time.monotonic() - start < a.duration:
            elapsed = time.monotonic() - start
            if process.poll() is not None:
                raise RuntimeError((root / 'compositor.log').read_text()[-7000:])
            if (root / 'pid').exists():
                try:
                    row = read_sample(int((root / 'pid').read_text()))
                    row['seconds'] = round(elapsed, 3)
                    samples.append(row)
                except FileNotFoundError:
                    pass
            time.sleep(0.05)

        text = (root / 'compositor.log').read_text()
        assert '[feather-network-check] complete: ok' in text, text[-7000:]
        calls = [json.loads(l) for l in calls_file.read_text().splitlines()]
        assert {'op': 'update_profile', 'path': '/test/saved/home', 'autoconnect': False, 'preserved_psk': True} in calls, calls
        assert {'op': 'delete_profile', 'path': '/test/saved/cafe'} in calls, calls
        assert {'op': 'disconnect_eth', 'device': '/test/eth0'} in calls, calls
        assert {'op': 'activate', 'profile': '/test/eth_profile', 'device': '/test/eth0', 'spec': '/'} in calls, calls
        assert {'op': 'deactivate', 'active': '/test/active/wg'} in calls, calls
        assert {'op': 'activate', 'profile': '/test/vpn/corp', 'device': '/', 'spec': '/'} in calls, calls
        assert {'op': 'add_and_activate', 'ssid': 'Neighbor WPA2', 'psk_len': 10, 'device': '/test/wlan0', 'spec': '/test/ap/neighbor'} in calls, calls
        assert {'op': 'deactivate', 'active': '/test/active/corp'} in calls, calls

        end = samples[-1]
        idle = max((s for s in samples if s['seconds'] <= end['seconds'] - 5.0), key=lambda s: s['seconds'])
        idle_ticks = end['ticks'] - idle['ticks']
        peak_rss_mb = round(max(s['anon_kib'] for s in samples) / 1024, 2)
        steady_rss_mb = round(end['anon_kib'] / 1024, 2)
        summary = {
            'samples': len(samples),
            'peak_rss_anon_mb': peak_rss_mb,
            'steady_rss_anon_mb': steady_rss_mb,
            'idle_window_s': round(end['seconds'] - idle['seconds'], 2),
            'idle_cpu_ticks': idle_ticks,
            'calls': len(calls),
        }
        (root / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
        print(json.dumps(summary, indent=2))
        assert steady_rss_mb <= 60.0, summary
        assert idle_ticks == 0, summary
    finally:
        try:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=3)
        except Exception:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except Exception:
                pass
