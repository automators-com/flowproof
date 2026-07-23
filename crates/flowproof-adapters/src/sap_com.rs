//! SAP GUI Scripting adapter (`sap-com`). The scripting API is the native
//! automation surface SAP ships with SAP GUI for Windows: every element has
//! a stable scripting id (`wnd[0]/usr/ctxtVBAK-AUART`), fields are set via
//! properties (not synthetic keystrokes), and buttons/menus are pressed
//! through the same interface the SAP test tools use. That id is this
//! provenance's NATIVE selector rung — deterministic replay needs nothing
//! else.
//!
//! Layering: [`SapAppDriver`] implements the platform-neutral `AppDriver`
//! on top of a small [`SapEngine`] trait. The Windows implementation talks
//! COM (late-bound `IDispatch`, the same calls a VBScript recording makes);
//! [`fake::FakeEngine`] is an in-memory SAP screen for tests, so the whole
//! record→trace→replay pipeline is exercised on every platform. Requires a
//! running SAP GUI with scripting enabled (`sapgui/user_scripting = TRUE`).

use std::time::Duration;

use flowproof_driver::{AppDriver, DriverError, KeyMod, PixelRect, UiaSelector};

/// One element of the SAP screen, as the scripting API describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SapElement {
    /// Session-relative scripting id (`wnd[0]/usr/txtNAME`).
    pub id: String,
    /// Scripting type (`GuiTextField`, `GuiButton`, `GuiCTextField`, …).
    pub kind: String,
    /// Technical name (`VBAK-AUART`).
    pub name: String,
    /// Visible text: the field value, button caption, or label text.
    pub text: String,
    /// Human-readable tooltip / quick info.
    pub tooltip: String,
    /// Whether the element accepts input right now.
    pub changeable: bool,
    /// Checkbox / radio state, when the element has one.
    pub selected: Option<bool>,
    /// Screen rectangle, when known.
    pub rect: Option<PixelRect>,
}

impl SapElement {
    /// Does this element answer to `needle` as a text anchor? The visible
    /// text, tooltip, and technical name all count — a spec says
    /// `the "Order Type" field`, the scripting API says `VBAK-AUART`.
    fn matches_text(&self, needle: &str) -> bool {
        let needle = needle.trim();
        !needle.is_empty()
            && (Self::without_required_marker(&self.text) == needle
                || Self::without_required_marker(&self.tooltip) == needle
                || self.name.trim() == needle)
    }

    /// SAP prefixes a mandatory field's label with `*` (e.g. `*Order Type`)
    /// — a screen-rendering convention, not part of the anchor a spec author
    /// would type or see when reading the field's label at a glance.
    fn without_required_marker(label: &str) -> &str {
        label.trim().strip_prefix('*').unwrap_or(label.trim()).trim()
    }
}

/// The scripting operations [`SapAppDriver`] needs from a live session.
/// Implemented by the COM bridge on Windows and by [`fake::FakeEngine`]
/// everywhere for tests.
pub trait SapEngine {
    /// Attach to a running, logged-in SAP GUI session. `connection` is the
    /// SAP Logon connection description to open if no session exists yet
    /// (empty = attach-only).
    fn connect(&mut self, connection: &str, timeout: Duration) -> Result<(), DriverError>;
    /// Look an element up by scripting id. `Ok(None)` = not on screen.
    fn find_by_id(&mut self, id: &str) -> Result<Option<SapElement>, DriverError>;
    /// Flattened walk of the visible session tree, top-down.
    fn walk(&mut self) -> Result<Vec<SapElement>, DriverError>;
    fn set_text(&mut self, id: &str, text: &str) -> Result<(), DriverError>;
    fn press(&mut self, id: &str) -> Result<(), DriverError>;
    fn select(&mut self, id: &str) -> Result<(), DriverError>;
    fn set_selected(&mut self, id: &str, selected: bool) -> Result<(), DriverError>;
    fn set_focus(&mut self, id: &str) -> Result<(), DriverError>;
    /// Send a virtual key to the active window (Enter = 0, F1–F12 = 1–12…).
    fn send_vkey(&mut self, vkey: u16) -> Result<(), DriverError>;
    fn screen_size(&mut self) -> Result<(u32, u32), DriverError>;
}

/// The command field every SAP session has — `Go to /nVA01` types here.
const OKCODE_FIELD: &str = "wnd[0]/tbar[0]/okcd";

/// SAP virtual key for a canonical key chord, per the scripting API's VKey
/// table: Enter=0, F1–F12=1–12, Shift+Fn=+12, Ctrl+Fn=+24, Ctrl+Shift+Fn=+36.
fn vkey_for(key: &str, modifiers: &[KeyMod]) -> Option<u16> {
    let shift = modifiers.contains(&KeyMod::Shift);
    let ctrl = modifiers.contains(&KeyMod::Ctrl);
    if modifiers
        .iter()
        .any(|m| matches!(m, KeyMod::Alt | KeyMod::Meta))
    {
        return None;
    }
    if key.eq_ignore_ascii_case("Enter") {
        return (!shift && !ctrl).then_some(0);
    }
    let f: u16 = key
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse().ok())
        .filter(|n| (1..=12).contains(n))?;
    Some(match (shift, ctrl) {
        (false, false) => f,
        (true, false) => f + 12,
        (false, true) => f + 24,
        (true, true) => f + 36,
    })
}

/// Scripting types a model can act on — the grounding set for `scene()`.
const INTERACTABLE_KINDS: &[&str] = &[
    "GuiTextField",
    "GuiCTextField",
    "GuiPasswordField",
    "GuiComboBox",
    "GuiButton",
    "GuiCheckBox",
    "GuiRadioButton",
    "GuiTab",
    "GuiMenu",
    "GuiOkCodeField",
];

/// `AppDriver` over a [`SapEngine`]. `E` is the COM bridge in production,
/// a fake in tests.
pub struct SapAppDriver<E: SapEngine> {
    engine: E,
}

impl<E: SapEngine> SapAppDriver<E> {
    pub fn with_engine(engine: E) -> Self {
        Self { engine }
    }

    /// Resolve a recorded selector to a live element: scripting id first
    /// (the native rung), then text anchor against the tree walk.
    fn resolve(&mut self, selector: &UiaSelector) -> Result<Option<SapElement>, DriverError> {
        if let Some(id) = &selector.automation_id {
            return self.engine.find_by_id(id);
        }
        let Some(needle) = selector.name.clone() else {
            return Ok(None);
        };
        let nth = selector.nth.unwrap_or(1).max(1) as usize;
        Ok(self
            .engine
            .walk()?
            .into_iter()
            .filter(|e| e.matches_text(&needle))
            .nth(nth - 1))
    }

    fn require(&mut self, selector: &UiaSelector) -> Result<SapElement, DriverError> {
        self.resolve(selector)?.ok_or_else(|| {
            DriverError::Uia(format!("sap: no element matches selector [{selector}]"))
        })
    }
}

#[cfg(windows)]
impl SapAppDriver<com::ComEngine> {
    /// The production driver: SAP GUI Scripting over COM.
    pub fn new() -> Result<Self, DriverError> {
        Ok(Self::with_engine(com::ComEngine::new()))
    }
}

impl<E: SapEngine> AppDriver for SapAppDriver<E> {
    fn launch(
        &mut self,
        command: &str,
        _window_name: &str,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        // `command` carries the SAP Logon connection description (may be
        // empty: attach to whatever logged-in session exists).
        self.engine.connect(command, timeout)
    }

    fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        Ok(self.resolve(selector)?.is_some())
    }

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let element = self.require(selector)?;
        match element.kind.as_str() {
            "GuiButton" => self.engine.press(&element.id),
            "GuiCheckBox" => self
                .engine
                .set_selected(&element.id, !element.selected.unwrap_or(false)),
            "GuiRadioButton" | "GuiTab" | "GuiMenu" => self.engine.select(&element.id),
            // Labels, list rows, everything else: focus is the closest
            // scripted equivalent of a click.
            _ => self.engine.set_focus(&element.id),
        }
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        let element = self.require(selector)?;
        Ok(if element.text.is_empty() {
            element.tooltip
        } else {
            element.text
        })
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        let element = self.require(selector)?;
        if !element.changeable {
            return Err(DriverError::Uia(format!(
                "sap: element '{}' ({}) is not changeable",
                element.id, element.kind
            )));
        }
        self.engine.set_focus(&element.id).ok();
        self.engine.set_text(&element.id, text)
    }

    fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        self.type_text(selector, "")
    }

    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
        let vkey = vkey_for(key, modifiers).ok_or_else(|| {
            DriverError::Uia(format!(
                "sap: no virtual key for '{key}' with these modifiers \
                 (supported: Enter, F1–F12, Shift/Ctrl+F1–F12)"
            ))
        })?;
        self.engine.send_vkey(vkey)
    }

    fn surface_text(&mut self) -> Result<String, DriverError> {
        // The desktop reading of "the surface": every visible text and
        // tooltip in the session tree, top-down — same contract as UIA
        // and the browser page text.
        let mut parts: Vec<String> = Vec::new();
        for element in self.engine.walk()? {
            if !element.text.trim().is_empty() {
                parts.push(element.text.trim().to_string());
            }
            if !element.tooltip.trim().is_empty() && element.tooltip.trim() != element.text.trim() {
                parts.push(element.tooltip.trim().to_string());
            }
        }
        Ok(parts.join("\n"))
    }

    fn navigate(&mut self, path: &str) -> Result<(), DriverError> {
        // `Go to /nVA01` — type the transaction code into the command
        // field and hit Enter, exactly how a user navigates SAP.
        self.engine.set_text(OKCODE_FIELD, path)?;
        self.engine.send_vkey(0)
    }

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        self.engine.screen_size()
    }

    fn capture(&mut self) -> Result<Option<image::RgbaImage>, DriverError> {
        #[cfg(windows)]
        {
            return flowproof_driver::gdi::capture_screen().map(Some);
        }
        #[cfg(not(windows))]
        Ok(None)
    }

    fn element_rect(&mut self, selector: &UiaSelector) -> Result<Option<PixelRect>, DriverError> {
        Ok(self.resolve(selector)?.and_then(|e| e.rect))
    }

    fn password_rects(&mut self) -> Result<Vec<PixelRect>, DriverError> {
        Ok(self
            .engine
            .walk()?
            .into_iter()
            .filter(|e| e.kind == "GuiPasswordField")
            .filter_map(|e| e.rect)
            .collect())
    }

    fn scene(&mut self) -> Result<Option<String>, DriverError> {
        // The grounding set for LLM authoring: interactable elements with
        // their `id:` TARGET TOKENS — same neutral contract as web/UIA.
        let mut entries: Vec<serde_json::Value> = Vec::new();
        for element in self.engine.walk()? {
            if entries.len() >= 100 {
                break;
            }
            if !INTERACTABLE_KINDS.contains(&element.kind.as_str()) || element.id.is_empty() {
                continue;
            }
            let mut entry = serde_json::json!({
                "target": format!("id:{}", element.id),
                "type": element.kind,
            });
            if !element.name.is_empty() {
                entry["name"] = element.name.into();
            }
            if !element.text.is_empty() {
                entry["text"] = element.text.into();
            }
            if !element.tooltip.is_empty() {
                entry["label"] = element.tooltip.into();
            }
            entries.push(entry);
        }
        serde_json::to_string(&entries)
            .map(Some)
            .map_err(|e| DriverError::Uia(format!("sap: serializing scene: {e}")))
    }
}

/// An in-memory SAP screen: the [`SapEngine`] tests script. Mirrors what
/// `flowproof_driver::mock::MockAppDriver` does for UIA — the full
/// record→trace→replay pipeline runs against it on any platform.
pub mod fake {
    use super::*;

    #[derive(Debug, Default)]
    pub struct FakeEngine {
        pub elements: Vec<SapElement>,
        /// `(button id, element id, new text)` — pressing the button sets
        /// the element's text, so flows have observable effects to assert.
        pub on_press: Vec<(String, String, String)>,
        /// Connection description passed to `connect` (None = never called).
        pub connected: Option<String>,
        pub pressed: Vec<String>,
        pub selected: Vec<String>,
        pub focused: Vec<String>,
        pub vkeys: Vec<u16>,
        pub set_texts: Vec<(String, String)>,
    }

    impl FakeEngine {
        pub fn with_elements(elements: Vec<SapElement>) -> Self {
            Self {
                elements,
                ..Self::default()
            }
        }

        fn index_of(&self, id: &str) -> Option<usize> {
            self.elements.iter().position(|e| e.id == id)
        }
    }

    impl SapEngine for FakeEngine {
        fn connect(&mut self, connection: &str, _timeout: Duration) -> Result<(), DriverError> {
            self.connected = Some(connection.to_string());
            Ok(())
        }

        fn find_by_id(&mut self, id: &str) -> Result<Option<SapElement>, DriverError> {
            Ok(self.index_of(id).map(|i| self.elements[i].clone()))
        }

        fn walk(&mut self) -> Result<Vec<SapElement>, DriverError> {
            Ok(self.elements.clone())
        }

        fn set_text(&mut self, id: &str, text: &str) -> Result<(), DriverError> {
            let i = self
                .index_of(id)
                .ok_or_else(|| DriverError::Uia(format!("fake sap: no element '{id}'")))?;
            self.elements[i].text = text.to_string();
            self.set_texts.push((id.to_string(), text.to_string()));
            Ok(())
        }

        fn press(&mut self, id: &str) -> Result<(), DriverError> {
            self.pressed.push(id.to_string());
            for (button, element, text) in self.on_press.clone() {
                if button == id {
                    if let Some(i) = self.index_of(&element) {
                        self.elements[i].text = text;
                    }
                }
            }
            Ok(())
        }

        fn select(&mut self, id: &str) -> Result<(), DriverError> {
            self.selected.push(id.to_string());
            Ok(())
        }

        fn set_selected(&mut self, id: &str, selected: bool) -> Result<(), DriverError> {
            let i = self
                .index_of(id)
                .ok_or_else(|| DriverError::Uia(format!("fake sap: no element '{id}'")))?;
            self.elements[i].selected = Some(selected);
            Ok(())
        }

        fn set_focus(&mut self, id: &str) -> Result<(), DriverError> {
            self.focused.push(id.to_string());
            Ok(())
        }

        fn send_vkey(&mut self, vkey: u16) -> Result<(), DriverError> {
            self.vkeys.push(vkey);
            Ok(())
        }

        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            Ok((1920, 1080))
        }
    }
}

/// SAP GUI Scripting over late-bound COM: exactly the `IDispatch` calls a
/// VBScript recording makes (`GetObject("SAPGUI")` → `GetScriptingEngine`
/// → `FindById` / property access), so behavior matches SAP's own tooling.
#[cfg(windows)]
pub mod com {
    use std::time::{Duration, Instant};

    use windows::core::{Interface, BSTR, GUID, PCWSTR};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CreateBindCtx, GetRunningObjectTable, IDispatch,
        COINIT_APARTMENTTHREADED, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET,
        DISPATCH_PROPERTYPUT, DISPPARAMS,
    };
    use windows::Win32::System::Ole::DISPID_PROPERTYPUT;
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    use super::{SapElement, SapEngine};
    use flowproof_driver::DriverError;

    fn com_err(context: &str, err: windows::core::Error) -> DriverError {
        DriverError::Uia(format!("sap-com: {context}: {err}"))
    }

    /// A late-bound COM object: name-based property/method access, the way
    /// scripting hosts drive the SAP GUI automation model.
    #[derive(Clone)]
    struct Disp(IDispatch);

    impl Disp {
        fn dispid(&self, name: &str) -> Result<i32, DriverError> {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            let names = [PCWSTR(wide.as_ptr())];
            let mut dispid = 0i32;
            unsafe {
                self.0
                    .GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, 0, &mut dispid)
            }
            .map_err(|e| com_err(&format!("member '{name}' not found"), e))?;
            Ok(dispid)
        }

        fn invoke(
            &self,
            name: &str,
            flags: DISPATCH_FLAGS,
            args: &mut [VARIANT],
        ) -> Result<VARIANT, DriverError> {
            let dispid = self.dispid(name)?;
            // IDispatch takes arguments right-to-left.
            args.reverse();
            let mut named_put = DISPID_PROPERTYPUT;
            let mut params = DISPPARAMS {
                rgvarg: args.as_mut_ptr(),
                cArgs: args.len() as u32,
                ..Default::default()
            };
            if flags == DISPATCH_PROPERTYPUT {
                params.rgdispidNamedArgs = &mut named_put;
                params.cNamedArgs = 1;
            }
            let mut result = VARIANT::default();
            unsafe {
                self.0.Invoke(
                    dispid,
                    &GUID::zeroed(),
                    0,
                    flags,
                    &params,
                    Some(&mut result),
                    None,
                    None,
                )
            }
            .map_err(|e| com_err(&format!("invoking '{name}'"), e))?;
            Ok(result)
        }

        fn get(&self, name: &str) -> Result<VARIANT, DriverError> {
            self.invoke(name, DISPATCH_PROPERTYGET, &mut [])
        }

        fn get_string(&self, name: &str) -> String {
            self.get(name)
                .ok()
                .and_then(|v| BSTR::try_from(&v).ok())
                .map(|b| b.to_string())
                .unwrap_or_default()
        }

        fn get_bool(&self, name: &str) -> Option<bool> {
            self.get(name).ok().and_then(|v| bool::try_from(&v).ok())
        }

        fn get_i32(&self, name: &str) -> Option<i32> {
            self.get(name).ok().and_then(|v| i32::try_from(&v).ok())
        }

        fn get_disp(&self, name: &str) -> Result<Disp, DriverError> {
            let value = self.get(name)?;
            IDispatch::try_from(&value)
                .map(Disp)
                .map_err(|e| com_err(&format!("'{name}' is not an object"), e))
        }

        fn call(&self, name: &str, mut args: Vec<VARIANT>) -> Result<VARIANT, DriverError> {
            self.invoke(name, DISPATCH_METHOD, &mut args)
        }

        fn call_disp(&self, name: &str, args: Vec<VARIANT>) -> Result<Disp, DriverError> {
            let value = self.call(name, args)?;
            IDispatch::try_from(&value)
                .map(Disp)
                .map_err(|e| com_err(&format!("'{name}' returned no object"), e))
        }

        fn put(&self, name: &str, value: VARIANT) -> Result<(), DriverError> {
            self.invoke(name, DISPATCH_PROPERTYPUT, &mut [value])
                .map(|_| ())
        }
    }

    /// The production [`SapEngine`]: holds the attached `GuiSession`.
    #[derive(Default)]
    pub struct ComEngine {
        session: Option<Disp>,
    }

    impl ComEngine {
        pub fn new() -> Self {
            Self::default()
        }

        fn session(&self) -> Result<&Disp, DriverError> {
            self.session
                .as_ref()
                .ok_or_else(|| DriverError::Uia("sap-com: not connected: call launch first".into()))
        }

        /// The session-relative scripting id (`wnd[0]/…`): the `Id`
        /// property is absolute (`/app/con[0]/ses[0]/wnd[0]/…`), but
        /// `FindById` and the trace address elements from the session.
        fn relative_id(absolute: &str) -> String {
            absolute
                .find("wnd[")
                .map(|i| absolute[i..].to_string())
                .unwrap_or_else(|| absolute.to_string())
        }

        fn element_info(element: &Disp) -> SapElement {
            let rect = match (
                element.get_i32("ScreenLeft"),
                element.get_i32("ScreenTop"),
                element.get_i32("Width"),
                element.get_i32("Height"),
            ) {
                (Some(x), Some(y), Some(w), Some(h)) => {
                    Some((x, y, w.unsigned_abs(), h.unsigned_abs()))
                }
                _ => None,
            };
            SapElement {
                id: Self::relative_id(&element.get_string("Id")),
                kind: element.get_string("Type"),
                name: element.get_string("Name"),
                text: element.get_string("Text"),
                tooltip: element.get_string("Tooltip"),
                changeable: element.get_bool("Changeable").unwrap_or(false),
                selected: element.get_bool("Selected"),
                rect,
            }
        }

        fn find_disp(&self, id: &str) -> Result<Option<Disp>, DriverError> {
            // FindById raises for unknown ids; that's the "not on screen"
            // signal, same as a failed UIA match.
            match self
                .session()?
                .call("FindById", vec![VARIANT::from(BSTR::from(id))])
            {
                Ok(value) => Ok(IDispatch::try_from(&value).ok().map(Disp)),
                Err(_) => Ok(None),
            }
        }

        fn walk_into(element: &Disp, depth: u32, out: &mut Vec<SapElement>) {
            if depth > 14 || out.len() >= 400 {
                return;
            }
            out.push(Self::element_info(element));
            // Leaves have no Children property — that's fine.
            let Ok(children) = element.get_disp("Children") else {
                return;
            };
            let count = children.get_i32("Count").unwrap_or(0);
            for i in 0..count {
                if let Ok(child) = children.call_disp("ElementAt", vec![VARIANT::from(i)]) {
                    Self::walk_into(&child, depth + 1, out);
                }
            }
        }
    }

    /// The name SAP GUI publishes itself under in the Running Object
    /// Table. An item moniker's display name carries its delimiter, so the
    /// entry may read `SAPGUI` or `!SAPGUI` depending on who created it;
    /// both mean the same object.
    const SAPGUI_ROT_NAME: &str = "SAPGUI";

    /// Bind to the running SAP GUI automation root by finding it in the
    /// Running Object Table.
    ///
    /// This is what `GetObject("SAPGUI")` reaches, spelled out. SAP does
    /// NOT register a `SAPGUI` ProgID - a real 7.60 install has no such
    /// key anywhere in HKCR - so `CLSIDFromProgID` + `GetActiveObject`,
    /// which this used to call, could never succeed against a live
    /// session. SAP publishes a plain ITEM MONIKER in the ROT instead.
    ///
    /// It enumerates rather than parsing the name into a moniker.
    /// `MkParseDisplayName("SAPGUI")` is the obvious shortcut and it
    /// returns MK_E_SYNTAX: with no ProgID to resolve, a bare word is not
    /// a parseable display name. Enumerating asks the ROT what is actually
    /// registered, which is also how the bug was diagnosed in the first
    /// place, and it does not care which delimiter the publisher chose.
    fn attach_to_sapgui() -> Result<IDispatch, DriverError> {
        let rot = unsafe { GetRunningObjectTable(0) }
            .map_err(|e| com_err("opening the Running Object Table", e))?;
        let running = unsafe { rot.EnumRunning() }
            .map_err(|e| com_err("enumerating the Running Object Table", e))?;
        let ctx =
            unsafe { CreateBindCtx(0) }.map_err(|e| com_err("creating a COM bind context", e))?;

        loop {
            let mut found = [const { None }; 1];
            let mut fetched = 0u32;
            unsafe { running.Next(&mut found, Some(&mut fetched)) }
                .ok()
                .map_err(|e| com_err("reading the Running Object Table", e))?;
            if fetched == 0 {
                break;
            }
            let Some(moniker) = found[0].take() else {
                continue;
            };
            let Ok(name) = (unsafe { moniker.GetDisplayName(&ctx, None) }) else {
                continue; // an entry we cannot name is not the one we want
            };
            // The name is COM-allocated; copy it out, then hand the memory
            // back whatever the comparison says.
            let display = unsafe { name.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(name.0 as *const _)) };

            if !display
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .eq_ignore_ascii_case(SAPGUI_ROT_NAME)
            {
                continue;
            }
            let unknown = unsafe { rot.GetObject(&moniker) }
                .map_err(|e| com_err("binding the SAPGUI object", e))?;
            return unknown
                .cast::<IDispatch>()
                .map_err(|_| DriverError::Uia("sap-com: SAPGUI object is not scriptable".into()));
        }
        Err(DriverError::Uia(
            "sap-com: SAP GUI is not running (no 'SAPGUI' in the Running Object Table)".into(),
        ))
    }

    impl SapEngine for ComEngine {
        fn connect(&mut self, connection: &str, timeout: Duration) -> Result<(), DriverError> {
            unsafe {
                // Per-thread; a prior init with another model just means
                // COM is already usable here.
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let deadline = Instant::now() + timeout;
            let mut opened = false;
            loop {
                let attempt = (|| -> Result<Option<Disp>, DriverError> {
                    let sapgui = Disp(attach_to_sapgui()?);
                    let engine = sapgui.call_disp("GetScriptingEngine", vec![])?;
                    let connections = engine.get_disp("Children")?;
                    if connections.get_i32("Count").unwrap_or(0) == 0 {
                        if !connection.is_empty() && !opened {
                            engine.call(
                                "OpenConnection",
                                vec![VARIANT::from(BSTR::from(connection)), VARIANT::from(true)],
                            )?;
                        }
                        return Ok(None); // keep waiting for a session
                    }
                    let conn = connections.call_disp("ElementAt", vec![VARIANT::from(0)])?;
                    let sessions = conn.get_disp("Children")?;
                    if sessions.get_i32("Count").unwrap_or(0) == 0 {
                        return Ok(None);
                    }
                    Ok(Some(
                        sessions.call_disp("ElementAt", vec![VARIANT::from(0)])?,
                    ))
                })();

                match attempt {
                    Ok(Some(session)) => {
                        self.session = Some(session);
                        return Ok(());
                    }
                    Ok(None) => opened = !connection.is_empty(),
                    Err(e) if Instant::now() >= deadline => {
                        return Err(DriverError::Uia(format!(
                            "{e} — start SAP Logon, log in, and enable scripting \
                             (sapgui/user_scripting = TRUE)"
                        )));
                    }
                    Err(_) => {}
                }
                if Instant::now() >= deadline {
                    return Err(DriverError::Uia(
                        "sap-com: timed out waiting for a logged-in SAP GUI session".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }

        fn find_by_id(&mut self, id: &str) -> Result<Option<SapElement>, DriverError> {
            Ok(self.find_disp(id)?.as_ref().map(Self::element_info))
        }

        fn walk(&mut self) -> Result<Vec<SapElement>, DriverError> {
            let mut out = Vec::new();
            Self::walk_into(self.session()?, 0, &mut out);
            Ok(out)
        }

        fn set_text(&mut self, id: &str, text: &str) -> Result<(), DriverError> {
            let element = self
                .find_disp(id)?
                .ok_or_else(|| DriverError::Uia(format!("sap-com: no element '{id}'")))?;
            element.put("Text", VARIANT::from(BSTR::from(text)))
        }

        fn press(&mut self, id: &str) -> Result<(), DriverError> {
            let element = self
                .find_disp(id)?
                .ok_or_else(|| DriverError::Uia(format!("sap-com: no element '{id}'")))?;
            element.call("Press", vec![]).map(|_| ())
        }

        fn select(&mut self, id: &str) -> Result<(), DriverError> {
            let element = self
                .find_disp(id)?
                .ok_or_else(|| DriverError::Uia(format!("sap-com: no element '{id}'")))?;
            element.call("Select", vec![]).map(|_| ())
        }

        fn set_selected(&mut self, id: &str, selected: bool) -> Result<(), DriverError> {
            let element = self
                .find_disp(id)?
                .ok_or_else(|| DriverError::Uia(format!("sap-com: no element '{id}'")))?;
            element.put("Selected", VARIANT::from(selected))
        }

        fn set_focus(&mut self, id: &str) -> Result<(), DriverError> {
            let element = self
                .find_disp(id)?
                .ok_or_else(|| DriverError::Uia(format!("sap-com: no element '{id}'")))?;
            element.call("SetFocus", vec![]).map(|_| ())
        }

        fn send_vkey(&mut self, vkey: u16) -> Result<(), DriverError> {
            let window = self
                .find_disp("wnd[0]")?
                .ok_or_else(|| DriverError::Uia("sap-com: no active window".into()))?;
            window
                .call("SendVKey", vec![VARIANT::from(i32::from(vkey))])
                .map(|_| ())
        }

        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
            Ok((w.unsigned_abs().max(1), h.unsigned_abs().max(1)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeEngine;
    use super::*;

    fn order_screen() -> Vec<SapElement> {
        vec![
            SapElement {
                id: "wnd[0]/tbar[0]/okcd".into(),
                kind: "GuiOkCodeField".into(),
                name: "okcd".into(),
                changeable: true,
                ..Default::default()
            },
            SapElement {
                id: "wnd[0]/usr/ctxtVBAK-AUART".into(),
                kind: "GuiCTextField".into(),
                name: "VBAK-AUART".into(),
                // SAP prefixes mandatory fields with `*` on the real screen.
                tooltip: "*Order Type".into(),
                changeable: true,
                ..Default::default()
            },
            SapElement {
                id: "wnd[0]/tbar[1]/btn[8]".into(),
                kind: "GuiButton".into(),
                name: "btn[8]".into(),
                text: "Continue".into(),
                tooltip: "Continue (Enter)".into(),
                ..Default::default()
            },
            SapElement {
                id: "wnd[0]/sbar".into(),
                kind: "GuiStatusbar".into(),
                name: "sbar".into(),
                ..Default::default()
            },
        ]
    }

    fn driver() -> SapAppDriver<FakeEngine> {
        SapAppDriver::with_engine(FakeEngine::with_elements(order_screen()))
    }

    #[test]
    fn type_by_scripting_id_sets_the_field() {
        let mut d = driver();
        d.type_text(
            &UiaSelector::automation_id("wnd[0]/usr/ctxtVBAK-AUART"),
            "ZOR",
        )
        .expect("types");
        assert_eq!(
            d.engine.set_texts,
            vec![("wnd[0]/usr/ctxtVBAK-AUART".to_string(), "ZOR".to_string())]
        );
        assert_eq!(
            d.read_text(&UiaSelector::automation_id("wnd[0]/usr/ctxtVBAK-AUART"))
                .expect("reads"),
            "ZOR"
        );
    }

    #[test]
    fn text_anchor_resolves_via_tooltip_and_technical_name() {
        let mut d = driver();
        let by_tooltip = UiaSelector {
            name: Some("Order Type".into()),
            ..Default::default()
        };
        assert!(d.element_exists(&by_tooltip).expect("walks"));
        d.type_text(&by_tooltip, "ZOR").expect("types via anchor");
        let by_name = UiaSelector {
            name: Some("VBAK-AUART".into()),
            ..Default::default()
        };
        assert_eq!(d.read_text(&by_name).expect("reads"), "ZOR");
    }

    #[test]
    fn invoke_presses_buttons_and_press_can_change_the_screen() {
        let mut d = driver();
        d.engine.on_press.push((
            "wnd[0]/tbar[1]/btn[8]".into(),
            "wnd[0]/sbar".into(),
            "Order 4711 saved".into(),
        ));
        d.invoke(&UiaSelector {
            name: Some("Continue".into()),
            ..Default::default()
        })
        .expect("presses");
        assert_eq!(d.engine.pressed, vec!["wnd[0]/tbar[1]/btn[8]".to_string()]);
        assert!(d
            .surface_text()
            .expect("surface")
            .contains("Order 4711 saved"));
    }

    #[test]
    fn unchangeable_elements_refuse_typing() {
        let mut d = driver();
        let err = d
            .type_text(&UiaSelector::automation_id("wnd[0]/tbar[1]/btn[8]"), "x")
            .expect_err("buttons are not changeable");
        assert!(err.to_string().contains("not changeable"));
    }

    #[test]
    fn navigate_types_the_transaction_into_okcd() {
        let mut d = driver();
        d.navigate("/nVA01").expect("navigates");
        assert_eq!(
            d.engine.set_texts,
            vec![("wnd[0]/tbar[0]/okcd".to_string(), "/nVA01".to_string())]
        );
        assert_eq!(d.engine.vkeys, vec![0]);
    }

    #[test]
    fn vkey_mapping_covers_enter_and_function_keys() {
        assert_eq!(vkey_for("Enter", &[]), Some(0));
        assert_eq!(vkey_for("F8", &[]), Some(8));
        assert_eq!(vkey_for("F3", &[KeyMod::Shift]), Some(15));
        assert_eq!(vkey_for("F1", &[KeyMod::Ctrl]), Some(25));
        assert_eq!(vkey_for("F2", &[KeyMod::Ctrl, KeyMod::Shift]), Some(38));
        assert_eq!(vkey_for("Escape", &[]), None);
        assert_eq!(vkey_for("Enter", &[KeyMod::Alt]), None);
    }

    #[test]
    fn scene_lists_interactables_with_id_tokens() {
        let mut d = driver();
        let scene = d.scene().expect("scene").expect("json");
        let entries: Vec<serde_json::Value> = serde_json::from_str(&scene).expect("parses");
        let targets: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["target"].as_str())
            .collect();
        assert!(targets.contains(&"id:wnd[0]/usr/ctxtVBAK-AUART"));
        assert!(targets.contains(&"id:wnd[0]/tbar[1]/btn[8]"));
        // The status bar is not interactable — grounding must not offer it.
        assert!(!scene.contains("sbar"));
    }
}
