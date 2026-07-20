"""A SAP GUI Scripting SIMULATOR: a real COM server shaped like SAP's
automation model, so flowproof's late-bound ComEngine can be exercised on
any Windows machine -- no SAP installation or license.

What it does, exactly like the real SAP GUI:
  * registers the ``SAPGUI`` ProgID (per-user, HKCU\\Software\\Classes --
    no admin needed) and puts a live object in the Running Object Table,
    which is where ``GetObject("SAPGUI")`` / ``GetActiveObject`` look;
  * serves ``GetScriptingEngine`` -> engine -> Children (connections) ->
    Children (sessions) -> ``FindById`` / property access / ``Press`` /
    ``SendVKey`` over IDispatch late binding;
  * reports ABSOLUTE element ids (``/app/con[0]/ses[0]/wnd[0]/...``) while
    accepting session-relative ids in FindById, mirroring real behavior;
  * raises a COM exception for unknown FindById ids (the engine treats
    that as "not on screen").

The screen is a small VA01-ish layout; pressing the Continue button posts
"Order 4711 saved" to the status bar so recorded flows have an observable
effect to assert.

Usage: python sap_simulator.py  (prints READY when attachable; exits on
its own after WATCHDOG_SECONDS as an orphan guard, or on Ctrl+C).
"""

import sys
import time
import winreg

import pythoncom
import pywintypes
import win32com.server.util
from win32com.server.exception import COMException

# Fixed, arbitrary CLSID for the simulator's SAPGUI object.
CLSID = "{7F3C9B1E-5A44-4D8B-9C2A-F10D8E401001}"
PROGID = "SAPGUI"
SESSION_PREFIX = "/app/con[0]/ses[0]/"
# Hard orphan guard only - generous enough that a slow CI runner's
# record + replay never outlives it (the test kills the process when
# it finishes; this exists for the case where it could not).
WATCHDOG_SECONDS = 1200


class Component:
    """One node of the scripting tree. COM names are resolved by the
    pywin32 policy from _public_methods_/_public_attrs_ (case-insensitive,
    like IDispatch name lookup)."""

    _public_methods_ = ["Press", "Select", "SetFocus"]
    _public_attrs_ = [
        "Id",
        "Type",
        "Name",
        "Text",
        "Tooltip",
        "Changeable",
        "ScreenLeft",
        "ScreenTop",
        "Width",
        "Height",
        "Children",
    ]

    def __init__(self, screen, rel_id, kind, name, text="", tooltip="", changeable=False):
        self._screen = screen
        self._rel_id = rel_id
        self.Id = SESSION_PREFIX + rel_id  # absolute, like real SAP
        self.Type = kind
        self.Name = name
        self.Text = text
        self.Tooltip = tooltip
        self.Changeable = changeable
        self.ScreenLeft = 10
        self.ScreenTop = 10
        self.Width = 120
        self.Height = 20
        self._children = []
        self.Children = wrap(Collection(self._children))

    def add(self, child):
        self._children.append(wrap(child))
        return child

    def Press(self):
        if self.Type != "GuiButton":
            raise COMException(desc="%s is not pressable" % self.Type)
        self._screen.on_press(self._rel_id)

    def Select(self):
        pass

    def SetFocus(self):
        pass


class Window(Component):
    _public_methods_ = Component._public_methods_ + ["SendVKey"]

    def SendVKey(self, vkey):
        self._screen.vkeys.append(int(vkey))


class Collection:
    _public_methods_ = ["ElementAt", "Item"]
    _public_attrs_ = ["Count"]

    def __init__(self, items):
        self._items = items

    @property
    def Count(self):
        return len(self._items)

    def ElementAt(self, index):
        return self._items[int(index)]

    Item = ElementAt


class Session(Component):
    _public_methods_ = Component._public_methods_ + ["FindById"]

    def __init__(self, screen):
        Component.__init__(self, screen, "ses", "GuiSession", "ses[0]")
        self.Id = "/app/con[0]/ses[0]"

    def FindById(self, element_id):
        element = self._screen.by_id.get(str(element_id))
        if element is None:
            # Real SAP raises for unknown ids; the engine maps this to
            # "not on screen".
            raise COMException(desc="control could not be found by id")
        return element


class Screen:
    """The VA01-ish screen plus its behavior (press effects, vkeys)."""

    def __init__(self):
        self.vkeys = []
        self.by_id = {}
        self.session = Session(self)
        window = Window(self, "wnd[0]", "GuiMainWindow", "wnd[0]", text="Create Standard Order")
        self.session.add(window)
        self._register("wnd[0]", window)

        def field(rel_id, kind, name, tooltip, changeable=True, text=""):
            component = Component(self, rel_id, kind, name, text, tooltip, changeable)
            window.add(component)
            self._register(rel_id, component)
            return component

        field("wnd[0]/tbar[0]/okcd", "GuiOkCodeField", "okcd", "Command field")
        field(
            "wnd[0]/usr/ctxtVBAK-AUART",
            "GuiCTextField",
            "VBAK-AUART",
            "Order Type",
        )
        field(
            "wnd[0]/usr/txtVBAK-KUNNR",
            "GuiTextField",
            "VBAK-KUNNR",
            "Customer",
        )
        field(
            "wnd[0]/tbar[1]/btn[8]",
            "GuiButton",
            "btn[8]",
            "Continue (Enter)",
            changeable=False,
            text="Continue",
        )
        self.sbar = field("wnd[0]/sbar", "GuiStatusbar", "sbar", "", changeable=False)

    def _register(self, rel_id, component):
        # FindById accepts both session-relative and absolute ids.
        wrapped = wrap(component)
        self.by_id[rel_id] = wrapped
        self.by_id[SESSION_PREFIX + rel_id] = wrapped

    def on_press(self, rel_id):
        if rel_id == "wnd[0]/tbar[1]/btn[8]":
            self.sbar.Text = "Order 4711 saved"


class Engine:
    _public_methods_ = ["OpenConnection"]
    _public_attrs_ = ["Children"]

    def __init__(self, screen):
        connection = Component(screen, "con", "GuiConnection", "con[0]")
        connection.Id = "/app/con[0]"
        connection.add(screen.session)
        self.Children = wrap(Collection([wrap(connection)]))

    def OpenConnection(self, description, sync=True):
        raise COMException(desc="simulator: a session is already running")


class SapGui:
    _public_methods_ = ["GetScriptingEngine"]
    _public_attrs_ = []

    def __init__(self, engine):
        self._engine = wrap(engine)

    def GetScriptingEngine(self):
        return self._engine


def wrap(instance):
    return win32com.server.util.wrap(instance)


def register_progid():
    """Per-user ProgID -> CLSID mapping, so CLSIDFromProgID("SAPGUI")
    resolves without SAP installed and without admin rights."""
    key = winreg.CreateKey(winreg.HKEY_CURRENT_USER, r"Software\Classes\%s\CLSID" % PROGID)
    winreg.SetValue(key, "", winreg.REG_SZ, CLSID)
    winreg.CloseKey(key)


def main():
    pythoncom.CoInitialize()
    register_progid()
    screen = Screen()
    sapgui = wrap(SapGui(Engine(screen)))
    handle = pythoncom.RegisterActiveObject(sapgui, pywintypes.IID(CLSID), 0)
    print("READY", flush=True)
    deadline = time.time() + WATCHDOG_SECONDS
    try:
        while time.time() < deadline:
            pythoncom.PumpWaitingMessages()
            time.sleep(0.02)
    except KeyboardInterrupt:
        pass
    finally:
        pythoncom.RevokeActiveObject(handle)
    return 0


if __name__ == "__main__":
    sys.exit(main())
