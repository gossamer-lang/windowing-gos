# A stand-in XSETTINGS manager: owns the default screen's selection and
# publishes the integer settings given as NAME=VALUE arguments, in the byte
# order --big or --little names, until it is stopped.
import signal
import struct
import sys

from Xlib import X, display

order = ">" if sys.argv[1] == "--big" else "<"
settings = [arg.split("=") for arg in sys.argv[2:]]
connection = display.Display()
window = connection.screen().root.create_window(0, 0, 1, 1, 0, X.CopyFromParent)
selection = connection.intern_atom(f"_XSETTINGS_S{connection.get_default_screen()}")
prop = connection.intern_atom("_XSETTINGS_SETTINGS")
body = b""
for name, value in settings:
    text = name.encode()
    body += struct.pack(order + "BxH", 0, len(text)) + text + b"\0" * (-len(text) % 4)
    body += struct.pack(order + "Ii", 0, int(value))
data = struct.pack(order + "B3xII", 1 if order == ">" else 0, 0, len(settings)) + body
window.change_property(prop, prop, 8, data)
window.set_selection_owner(selection, X.CurrentTime)
connection.sync()
print("ready", flush=True)
signal.pause()
