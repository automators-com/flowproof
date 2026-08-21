"""A SAP GUI Scripting SIMULATOR: a real COM server shaped like SAP's
automation model, so flowproof's late-bound ComEngine can be exercised on
any Windows machine -- no SAP installation or license.

What it does, exactly like the real SAP GUI:
  * publishes itself in the Running Object Table under the ITEM MONIKER
    ``SAPGUI``, which is what ``GetObject("SAPGUI")`` resolves and what a
    real SAP GUI install actually registers. It deliberately does NOT
    register a ``SAPGUI`` ProgID: a real 7.60 install has no such key
    anywhere in HKCR, and pretending otherwise is what hid issue #85 -
    the engine attached via ``CLSIDFromProgID`` for as long as the only
    thing it was ever tested against was a simulator that registered one;
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

It models three SAP screen SHAPES, not one, because a fixture with a single
flat window cannot fail when window scoping or nesting breaks:

  * the ordinary ``wnd[0]`` screen — flat fields under ``usr``;
  * a ``GuiTableControl`` whose cells are CHILDREN of the table and carry
    real SAP cell ids (``.../ctxtVBAP-MATNR[0,1]``), so the walk has to
    recurse past depth two and FindById has to survive brackets and commas;
  * a ``wnd[1]`` GuiModalWindow, opened by Back and closed by either of its
    buttons. While it is open the session tree holds BOTH windows, exactly
    as real SAP does, and the two windows carry deliberately disjoint text
    so a caller can tell which one it is reading.

The shapes are the extension points for the remaining SAP surfaces: an ALV
grid is another nested container under ``usr``, and an F4 search help is
another ``wnd[1]`` modal. Neither is modelled here yet.

Usage: python sap_simulator.py  (prints READY when attachable; exits on
its own after WATCHDOG_SECONDS as an orphan guard, or on Ctrl+C).
"""

import sys
import time

import pythoncom
import win32com.server.util
from win32com.server.exception import COMException

# The ROT item-moniker name real SAP GUI publishes itself under.
ROT_NAME = "SAPGUI"
SESSION_PREFIX = "/app/con[0]/ses[0]/"
# The classic VA01 item table control, named as the real screen names it.
ITEM_TABLE = "wnd[0]/usr/tblSAPMV45ATCTRL_U_ERF_AUFTRAG"
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
        "MessageType",
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
        self.MessageType = ""
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
        self._screen.on_vkey(int(vkey))


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


class SessionInfo:
    """`GuiSession.Info`, and specifically its `User`.

    The engine uses a non-empty `User` to tell a logged-in session from one
    sitting at the SAP login screen: a session object exists as soon as its
    window opens, login screen included, so existence alone proves nothing.
    Real SAP reports transaction `S000` and an empty user until someone
    authenticates.

    The simulator therefore has to present a session that is actually logged
    in, because that is what the test claims to exercise - "record through the
    production COM engine against a running, logged-in session". A simulator
    that reported an empty user would be asking the engine to relax the check
    rather than asking the test to be honest.
    """

    _public_methods_ = []
    _public_attrs_ = ["User", "Client", "Transaction", "SystemName"]

    def __init__(self, user="FLOWPROOF", client="001", transaction="VA01",
                 system="SIM"):
        self.User = user
        self.Client = client
        self.Transaction = transaction
        self.SystemName = system


class Session(Component):
    _public_methods_ = Component._public_methods_ + ["FindById"]
    _public_attrs_ = Component._public_attrs_ + ["Info"]

    def __init__(self, screen, user="FLOWPROOF", system="SIM"):
        Component.__init__(self, screen, "ses", "GuiSession", "ses[0]")
        self.Id = "/app/con[0]/ses[0]"
        # Wrapped, like every other nested object here (`Children`, `child`,
        # `connection`). An unwrapped Python instance is not dispatchable, so
        # the engine's `get_disp("Info")` would fail and the session would look
        # exactly as "not logged in" as before.
        self._info = SessionInfo(user=user, system=system)
        self.Info = wrap(self._info)

    def FindById(self, element_id):
        element = self._screen.by_id.get(str(element_id))
        if element is None:
            # Real SAP raises for unknown ids; the engine maps this to
            # "not on screen".
            raise COMException(desc="control could not be found by id")
        return element


class Screen:
    """The VA01-ish screen plus its behavior (press effects, vkeys)."""

    def __init__(self, user="FLOWPROOF", title="Create Standard Order", order_screen=True):
        self.vkeys = []
        self.by_id = {}
        self.modal = None
        self.session = Session(self, user=user, system="SIM" if order_screen else "OTHER")
        self.window = Window(self, "wnd[0]", "GuiMainWindow", "wnd[0]", text=title)
        self.session.add(self.window)
        self._register("wnd[0]", self.window)
        field = self.add_field

        field("wnd[0]/tbar[0]/okcd", "GuiOkCodeField", "okcd", "Command field")
        # Standard SAP login controls. The desired simulated connection starts
        # logged out so the Rust adapter must fill these and submit Enter.
        self.client_field = field(
            "wnd[0]/usr/txtRSYST-MANDT", "GuiTextField", "RSYST-MANDT", "Client"
        )
        self.user_field = field(
            "wnd[0]/usr/txtRSYST-BNAME", "GuiTextField", "RSYST-BNAME", "User"
        )
        self.password_field = field(
            "wnd[0]/usr/pwdRSYST-BCODE", "GuiPasswordField", "RSYST-BCODE", "Password"
        )
        self.language_field = field(
            "wnd[0]/usr/txtRSYST-LANGU", "GuiTextField", "RSYST-LANGU", "Language"
        )
        if order_screen:
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
            field(
                "wnd[0]/tbar[0]/btn[3]",
                "GuiButton",
                "btn[3]",
                "Back (F3)",
                changeable=False,
            )
            # The classic item table. Cells hang off the TABLE, not off the
            # window, and their ids carry SAP's `[column,row]` suffix - the
            # two things a flat fixture never made anyone get right.
            table = field(
                ITEM_TABLE,
                "GuiTableControl",
                "SAPMV45ATCTRL_U_ERF_AUFTRAG",
                "Item overview",
                changeable=False,
            )
            for row in (0, 1):
                field(
                    "%s/ctxtVBAP-MATNR[0,%d]" % (ITEM_TABLE, row),
                    "GuiCTextField",
                    "VBAP-MATNR",
                    "Material",
                    parent=table,
                )
                field(
                    "%s/txtVBAP-KWMENG[1,%d]" % (ITEM_TABLE, row),
                    "GuiTextField",
                    "VBAP-KWMENG",
                    "Order Quantity",
                    parent=table,
                )
        self.sbar = field("wnd[0]/sbar", "GuiStatusbar", "sbar", "", changeable=False)

    def add_field(
        self, rel_id, kind, name, tooltip, changeable=True, text="", parent=None
    ):
        """Register one control. `parent` defaults to `wnd[0]`; passing a
        container (a table, a modal) is what makes the tree deeper than
        one level, which is the shape an ALV grid would reuse."""
        component = Component(self, rel_id, kind, name, text, tooltip, changeable)
        (parent or self.window).add(component)
        self._register(rel_id, component)
        return component

    def _register(self, rel_id, component):
        # FindById accepts both session-relative and absolute ids.
        wrapped = wrap(component)
        self.by_id[rel_id] = wrapped
        self.by_id[SESSION_PREFIX + rel_id] = wrapped

    def open_modal(self):
        """Put a `wnd[1]` popup over the main screen, the way SAP asks
        whether to save on Back.

        The main window STAYS in the session tree while this is open -
        that is the whole point. Its text ("Create Standard Order", the
        field labels) is text the user cannot act on until the popup goes
        away, so anything that reads the session flat will read it anyway.
        """
        if self.modal is not None:
            return
        modal = Window(
            self, "wnd[1]", "GuiModalWindow", "wnd[1]", text="Exit Processing"
        )
        self.session.add(modal)
        self._register("wnd[1]", modal)
        self.modal = modal
        self.add_field(
            "wnd[1]/usr/txtMESSTXT1",
            "GuiTextField",
            "MESSTXT1",
            "",
            changeable=False,
            text="Do you want to save your data?",
            parent=modal,
        )
        for suffix, caption in (("1", "Yes"), ("2", "No")):
            self.add_field(
                "wnd[1]/usr/btnSPOP-OPTION" + suffix,
                "GuiButton",
                "SPOP-OPTION" + suffix,
                "",
                changeable=False,
                text=caption,
                parent=modal,
            )

    def close_modal(self):
        """Dismiss the popup: `wnd[1]` and everything under it leaves the
        session tree, which is what makes the main window reachable again."""
        if self.modal is None:
            return
        self.session._children.pop()  # wnd[1] is the last child added
        for key in [k for k in self.by_id if "wnd[1]" in k]:
            del self.by_id[key]
        self.modal = None

    def on_press(self, rel_id):
        if rel_id == "wnd[0]/tbar[1]/btn[8]":
            self.sbar.Text = "Order 4711 saved"
        elif rel_id == "wnd[0]/tbar[0]/btn[3]":
            self.open_modal()
        elif rel_id.startswith("wnd[1]/usr/btnSPOP-OPTION"):
            self.sbar.Text = (
                "Document 4711 saved"
                if rel_id.endswith("1")
                else "Processing was ended"
            )
            self.close_modal()

    def on_vkey(self, vkey):
        if (
            vkey == 0
            and not self.session._info.User
            and self.user_field.Text == "SIMUSER"
            and self.password_field.Text == "SIMPASS"
        ):
            self.session._info.User = "SIMUSER"
            self.session._info.Client = self.client_field.Text or "001"
            self.window.Text = "Create Standard Order"


class Connection(Component):
    _public_attrs_ = Component._public_attrs_ + ["Description", "SystemName"]

    def __init__(self, screen, description, system):
        Component.__init__(self, screen, "con", "GuiConnection", description)
        self.Id = "/app/con[0]"
        self.Description = description
        self.SystemName = system
        self.add(screen.session)


class Engine:
    _public_methods_ = ["OpenConnection"]
    _public_attrs_ = ["Children"]

    def __init__(self, screen):
        # An unrelated connection deliberately comes first. Flowproof must
        # select the connection requested by the flow instead of blindly
        # attaching to Children[0].
        unrelated_screen = Screen(
            user="OTHERUSER", title="SAP Easy Access - Other", order_screen=False
        )
        unrelated = Connection(unrelated_screen, "Other system", "OTHER")
        desired = Connection(screen, "SIM", "SIM")
        self.Children = wrap(Collection([wrap(unrelated), wrap(desired)]))

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


def register_in_rot(obj):
    """Publish `obj` in the Running Object Table under the item moniker
    "SAPGUI" - the mechanism real SAP GUI uses, and the one
    GetObject("SAPGUI") goes through.

    ROTFLAGS_REGISTRATIONKEEPSALIVE (1) keeps the entry alive while this
    process holds the registration, which is what a real session does.
    """
    moniker = pythoncom.CreateItemMoniker("!", ROT_NAME)
    return pythoncom.GetRunningObjectTable().Register(1, obj, moniker)


def main():
    pythoncom.CoInitialize()
    screen = Screen(user="", title="SAP Logon", order_screen=True)
    sapgui = wrap(SapGui(Engine(screen)))
    handle = register_in_rot(sapgui)
    print("READY", flush=True)
    deadline = time.time() + WATCHDOG_SECONDS
    try:
        while time.time() < deadline:
            pythoncom.PumpWaitingMessages()
            time.sleep(0.02)
    except KeyboardInterrupt:
        pass
    finally:
        pythoncom.GetRunningObjectTable().Revoke(handle)
    return 0


if __name__ == "__main__":
    sys.exit(main())
