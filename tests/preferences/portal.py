# A stand-in for the XDG desktop portal's Settings interface: it answers the
# appearance and GNOME keys, and on a "change" line on standard input switches
# to high contrast with no animations and a larger interface font, and
# announces it.
import sys
from gi.repository import Gio, GLib

XML = """<node><interface name='org.freedesktop.portal.Settings'>
<method name='ReadOne'><arg type='s' direction='in'/><arg type='s' direction='in'/><arg type='v' direction='out'/></method>
<signal name='SettingChanged'><arg type='s'/><arg type='s'/><arg type='v'/></signal>
<property name='version' type='u' access='read'/></interface></node>"""
values = {
    ("org.freedesktop.appearance", "accent-color"): GLib.Variant("(ddd)", (0.2, 0.4, 1.0)),
    ("org.gnome.desktop.interface", "enable-animations"): GLib.Variant("b", True),
    ("org.gnome.desktop.interface", "text-scaling-factor"): GLib.Variant("d", 1.25),
    ("org.gnome.desktop.interface", "font-name"): GLib.Variant("s", "Cantarell 11"),
}
connection = None

def call(conn, sender, path, iface, method, params, invocation):
    key = tuple(params.unpack())
    if key in values:
        invocation.return_value(GLib.Variant("(v)", (values[key],)))
    else:
        invocation.return_dbus_error("org.freedesktop.portal.Error.NotFound", "Requested setting not found")

def prop(conn, sender, path, iface, name):
    return GLib.Variant("u", 2)

def acquired(conn, name):
    global connection
    connection = conn
    print("ready", flush=True)

def bus(conn, name):
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    conn.register_object("/org/freedesktop/portal/desktop", node.interfaces[0], call, prop, None)

def line(source, condition):
    text = sys.stdin.readline()
    if not text:
        loop.quit()
        return False
    if text.strip() == "change":
        changed = {
            ("org.freedesktop.appearance", "contrast"): GLib.Variant("u", 1),
            ("org.gnome.desktop.interface", "enable-animations"): GLib.Variant("b", False),
            ("org.gnome.desktop.interface", "font-name"): GLib.Variant("s", "Cantarell Bold 13"),
        }
        values.update(changed)
        for (namespace, key), value in changed.items():
            connection.emit_signal(None, "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings", "SettingChanged", GLib.Variant("(ssv)", (namespace, key, value)))
        connection.flush_sync(None)
    return True

loop = GLib.MainLoop()
Gio.bus_own_name(Gio.BusType.SESSION, "org.freedesktop.portal.Desktop", 0, bus, acquired, None)
GLib.io_add_watch(sys.stdin, GLib.IO_IN | GLib.IO_HUP, line)
loop.run()
