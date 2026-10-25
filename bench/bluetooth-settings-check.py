#!/usr/bin/env python3
"""Owned nested compositor + private D-Bus mock BlueZ (org.bluez).

Never touches the system D-Bus or real BlueZ: DBUS_SYSTEM_BUS_ADDRESS is pointed
at the private dbus-run-session bus. Exercises Control Center -> Bluetooth page
+ StartDiscovery, unnamed BLE beacon filtering, Control Center -> System Settings
Bluetooth navigation, pairing + trusting + connecting an unpaired device,
disconnecting an active audio headset, forgetting a paired keyboard via
Adapter1.RemoveDevice, error persistence across background snapshots, StopDiscovery,
RssAnon <= 60 MB and 0 idle CPU ticks/5 s.
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

if len(sys.argv) > 1 and sys.argv[1] == '--mock-bluez':
    from gi.repository import Gio, GLib

    log_path = Path(sys.argv[2])
    ready_path = Path(sys.argv[3])

    def record(entry):
        with log_path.open('a') as f:
            f.write(json.dumps(entry) + '\n')

    state = {
        'powered': True,
        'discovering': False,
        'pods_fail_once': False,
        'devices': {
            '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01': {
                'Address': 'AA:BB:CC:DD:EE:01',
                'Name': 'AirPods Pro',
                'Alias': 'AirPods Pro',
                'Icon': 'audio-headphones',
                'Paired': True,
                'Trusted': True,
                'Connected': True,
                'Battery': 88,
            },
            '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02': {
                'Address': 'AA:BB:CC:DD:EE:02',
                'Name': 'Keychron K2',
                'Alias': 'Keychron K2',
                'Icon': 'input-keyboard',
                'Paired': True,
                'Trusted': True,
                'Connected': False,
                'Battery': 64,
            },
            '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03': {
                'Address': 'AA:BB:CC:DD:EE:03',
                'Name': 'MX Master 3S',
                'Alias': 'MX Master 3S',
                'Icon': 'input-mouse',
                'Paired': False,
                'Trusted': False,
                'Connected': False,
                'Battery': None,
            },
            '/org/bluez/hci0/dev_AA_BB_CC_00_11_22': {
                'Address': 'AA:BB:CC:00:11:22',
                'Name': '',
                'Alias': 'AA-BB-CC-00-11-22',
                'Icon': '',
                'Paired': False,
                'Trusted': False,
                'Connected': False,
                'Battery': None,
            },
        },
    }

    def emit_props(conn, path, iface, changed):
        conn.emit_signal(
            None,
            path,
            'org.freedesktop.DBus.Properties',
            'PropertiesChanged',
            GLib.Variant('(sa{sv}as)', (iface, changed, [])),
        )

    ROOT_XML = """
    <node>
      <interface name="org.freedesktop.DBus.ObjectManager">
        <method name="GetManagedObjects">
          <arg type="a{oa{sa{sv}}}" direction="out"/>
        </method>
        <signal name="InterfacesAdded">
          <arg type="o"/><arg type="a{sa{sv}}"/>
        </signal>
        <signal name="InterfacesRemoved">
          <arg type="o"/><arg type="as"/>
        </signal>
      </interface>
    </node>"""

    ADAPTER_XML = """
    <node>
      <interface name="org.bluez.Adapter1">
        <method name="StartDiscovery"/>
        <method name="StopDiscovery"/>
        <method name="RemoveDevice"><arg type="o" direction="in"/></method>
        <property name="Powered" type="b" access="readwrite"/>
        <property name="Discovering" type="b" access="read"/>
      </interface>
    </node>"""

    DEVICE_XML = """
    <node>
      <interface name="org.bluez.Device1">
        <method name="Connect"/>
        <method name="Disconnect"/>
        <method name="Pair"/>
        <property name="Address" type="s" access="read"/>
        <property name="Name" type="s" access="read"/>
        <property name="Alias" type="s" access="read"/>
        <property name="Icon" type="s" access="read"/>
        <property name="Paired" type="b" access="read"/>
        <property name="Trusted" type="b" access="readwrite"/>
        <property name="Connected" type="b" access="read"/>
      </interface>
      <interface name="org.bluez.Battery1">
        <property name="Percentage" type="y" access="read"/>
      </interface>
    </node>"""

    conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    registrations = {}

    def adapter_props():
        return {
            'Powered': GLib.Variant('b', state['powered']),
            'Discovering': GLib.Variant('b', state['discovering']),
        }

    def device_props(d):
        return {
            'Address': GLib.Variant('s', d['Address']),
            'Name': GLib.Variant('s', d['Name']),
            'Alias': GLib.Variant('s', d['Alias']),
            'Icon': GLib.Variant('s', d['Icon']),
            'Paired': GLib.Variant('b', d['Paired']),
            'Trusted': GLib.Variant('b', d['Trusted']),
            'Connected': GLib.Variant('b', d['Connected']),
        }

    def managed_objects():
        objs = {
            '/org/bluez/hci0': {
                'org.bluez.Adapter1': adapter_props(),
            }
        }
        for path, d in state['devices'].items():
            ifaces = {'org.bluez.Device1': device_props(d)}
            if d['Battery'] is not None:
                ifaces['org.bluez.Battery1'] = {
                    'Percentage': GLib.Variant('y', d['Battery'])
                }
            objs[path] = ifaces
        return objs

    def on_get_prop(c, sender, path, iface, prop):
        if path == '/org/bluez/hci0' and iface == 'org.bluez.Adapter1':
            return adapter_props().get(prop)
        if path in state['devices']:
            d = state['devices'][path]
            if iface == 'org.bluez.Device1':
                return device_props(d).get(prop)
            if iface == 'org.bluez.Battery1' and d['Battery'] is not None and prop == 'Percentage':
                return GLib.Variant('y', d['Battery'])
        return None

    def on_set_prop(c, sender, path, iface, prop, value):
        if path == '/org/bluez/hci0' and iface == 'org.bluez.Adapter1' and prop == 'Powered':
            state['powered'] = bool(value.unpack())
            if not state['powered']:
                state['discovering'] = False
            record({'op': 'set_powered', 'powered': state['powered']})
            emit_props(c, path, iface, adapter_props())
            return True
        if path in state['devices'] and iface == 'org.bluez.Device1' and prop == 'Trusted':
            state['devices'][path]['Trusted'] = bool(value.unpack())
            record({'op': 'set_trusted', 'path': path, 'trusted': state['devices'][path]['Trusted']})
            emit_props(c, path, iface, {'Trusted': GLib.Variant('b', state['devices'][path]['Trusted'])})
            return True
        return False

    def on_method(c, sender, path, iface, method, params, invocation):
        args = params.unpack()
        if path == '/' and iface == 'org.freedesktop.DBus.ObjectManager' and method == 'GetManagedObjects':
            invocation.return_value(GLib.Variant('(a{oa{sa{sv}}})', (managed_objects(),)))
            return
        if path == '/org/bluez/hci0' and iface == 'org.bluez.Adapter1':
            if method == 'StartDiscovery':
                state['discovering'] = True
                record({'op': 'start_discovery'})
                emit_props(c, path, iface, {'Discovering': GLib.Variant('b', True)})
                invocation.return_value(None)
                return
            if method == 'StopDiscovery':
                state['discovering'] = False
                record({'op': 'stop_discovery'})
                emit_props(c, path, iface, {'Discovering': GLib.Variant('b', False)})
                invocation.return_value(None)
                return
            if method == 'RemoveDevice':
                (dev_path,) = args
                record({'op': 'remove_device', 'path': dev_path})
                state['devices'].pop(dev_path, None)
                for reg_id in registrations.pop(dev_path, []):
                    c.unregister_object(reg_id)
                c.emit_signal(
                    None,
                    '/',
                    'org.freedesktop.DBus.ObjectManager',
                    'InterfacesRemoved',
                    GLib.Variant('(oas)', (dev_path, ['org.bluez.Device1', 'org.bluez.Battery1'])),
                )
                invocation.return_value(None)
                return
        if path in state['devices'] and iface == 'org.bluez.Device1':
            d = state['devices'][path]
            if method == 'Pair':
                d['Paired'] = True
                record({'op': 'pair', 'path': path})
                emit_props(c, path, iface, {'Paired': GLib.Variant('b', True)})
                invocation.return_value(None)
                return
            if method == 'Connect':
                record({'op': 'connect', 'path': path})
                if path.endswith('_01') and state['pods_fail_once']:
                    state['pods_fail_once'] = False
                    def bump():
                        emit_props(c, '/org/bluez/hci0', 'org.bluez.Adapter1', adapter_props())
                        return False
                    GLib.timeout_add(40, bump)
                    invocation.return_dbus_error('org.bluez.Error.Failed', 'Наушники вне зоны действия')
                    return
                d['Connected'] = True
                emit_props(c, path, iface, {'Connected': GLib.Variant('b', True)})
                invocation.return_value(None)
                return
            if method == 'Disconnect':
                record({'op': 'disconnect', 'path': path})
                d['Connected'] = False
                if path.endswith('_01'):
                    state['pods_fail_once'] = True
                emit_props(c, path, iface, {'Connected': GLib.Variant('b', False)})
                invocation.return_value(None)
                return
        invocation.return_dbus_error('org.freedesktop.DBus.Error.UnknownMethod', method)

    for iface_info in Gio.DBusNodeInfo.new_for_xml(ROOT_XML).interfaces:
        conn.register_object('/', iface_info, on_method, on_get_prop, on_set_prop)
    for iface_info in Gio.DBusNodeInfo.new_for_xml(ADAPTER_XML).interfaces:
        conn.register_object('/org/bluez/hci0', iface_info, on_method, on_get_prop, on_set_prop)

    dev_ifaces = Gio.DBusNodeInfo.new_for_xml(DEVICE_XML).interfaces
    for dev_path in list(state['devices'].keys()):
        registrations[dev_path] = [
            conn.register_object(dev_path, iface_info, on_method, on_get_prop, on_set_prop)
            for iface_info in dev_ifaces
        ]

    loop = GLib.MainLoop()
    Gio.bus_own_name_on_connection(
        conn,
        'org.bluez',
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
p.add_argument('--duration', type=float, default=12.5)
p.add_argument('--protocol-summary', action='store_true')
a = p.parse_args()
if a.duration < 11.5:
    p.error('--duration must be at least 11.5 seconds')

root = (a.output or Path(tempfile.mkdtemp(prefix='feather-bluetooth-'))).resolve()
root.mkdir(parents=True, exist_ok=True)
calls_file = root / 'calls.jsonl'
calls_file.write_text('')
ready_file = root / 'bluez.ready'
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
    FEATHER_DEBUG_BLUETOOTH_SETTINGS='1',
    RUST_LOG='feather_de=debug,smithay=warn',
)
if a.protocol_summary:
    env['WAYLAND_DEBUG'] = 'client'
if a.screenshot:
    env.update(
        FEATHER_DEBUG_SCREENSHOT=str(root / 'frame.png'),
        FEATHER_DEBUG_SCREENSHOT_MS='6000',
    )

shell_cmd = (
    'export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"; '
    'python3 "$1" --mock-bluez "$2" "$3" & '
    'bz_pid=$!; '
    'while [ ! -f "$3" ]; do sleep 0.02; done; '
    'echo $$ > "$4"; '
    'trap "kill $bz_pid 2>/dev/null || true" EXIT; '
    'exec "$5" --windowed'
)
command = [
    'dbus-run-session',
    '--',
    'sh',
    '-c',
    shell_cmd,
    'bluetooth-settings-check',
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
        assert '[feather-bluetooth-check] stop discovery + complete: ok' in text, text[-7000:]
        calls = [json.loads(l) for l in calls_file.read_text().splitlines()]
        assert {'op': 'start_discovery'} in calls, calls
        assert {'op': 'pair', 'path': '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03'} in calls, calls
        assert {'op': 'set_trusted', 'path': '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03', 'trusted': True} in calls, calls
        assert {'op': 'connect', 'path': '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03'} in calls, calls
        assert {'op': 'disconnect', 'path': '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01'} in calls, calls
        assert {'op': 'remove_device', 'path': '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02'} in calls, calls
        assert {'op': 'connect', 'path': '/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01'} in calls, calls
        assert {'op': 'stop_discovery'} in calls, calls

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
