//! The multi-surface driver: one [`SurfaceRegistry`] stands where a single
//! driver otherwise would, routing every call to the ACTIVE surface of a
//! multi-surface flow (docs/multi-surface.md). Exactly one is active at a
//! time — SAP GUI scripting, UIA and vision all inject real input into
//! the FOREGROUND window, so sequential activation is correctness, not a
//! simplification. Launch is lazy; a launched surface is kept alive.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::app::{
    AppDriver, AppTarget, CellHints, CookieProbe, DebugBundle, DialogArm, FiredDialog, FrameProbe,
    FrameQuery, KeyMod, PixelRect, ScopeHints, ScrollTo, UiaSelector, WebBrowserConfig, WebMock,
    WebSession,
};
use crate::DriverError;

/// Builds the driver for a surface at its FIRST activation. What kind a
/// surface gets (CDP, SAP COM, UIA) stays the caller's decision — the
/// CLI closes over its `driver_for`, tests close over mocks.
pub type SurfaceFactory = Box<dyn FnMut(&str) -> Result<Box<dyn AppDriver>, DriverError>>;

struct SurfaceSlot {
    target: AppTarget,
    /// `None` until the surface's first activation launches it.
    driver: Option<Box<dyn AppDriver>>,
}

pub struct SurfaceRegistry {
    surfaces: BTreeMap<String, SurfaceSlot>,
    active: Option<String>,
    factory: SurfaceFactory,
    launch_timeout: Duration,
}

impl SurfaceRegistry {
    /// `surfaces` maps each name to the target its driver launches (or
    /// attaches to); resolution of `${VAR}` refs happened before this.
    pub fn new(
        surfaces: impl IntoIterator<Item = (String, AppTarget)>,
        factory: SurfaceFactory,
        launch_timeout: Duration,
    ) -> Self {
        let slot = |target| SurfaceSlot {
            target,
            driver: None,
        };
        Self {
            surfaces: surfaces
                .into_iter()
                .map(|(name, target)| (name, slot(target)))
                .collect(),
            active: None,
            factory,
            launch_timeout,
        }
    }

    /// Make `name` the active surface: build + launch its driver on first
    /// activation, RE-launch (launch-or-attach is [`AppDriver::launch`]'s
    /// contract) on return visits so its window is foreground again. The
    /// parked surface needs no call: foregrounding the next parks it.
    pub fn activate(&mut self, name: &str) -> Result<(), DriverError> {
        let Some(slot) = self.surfaces.get_mut(name) else {
            let declared: Vec<&str> = self.surfaces.keys().map(String::as_str).collect();
            return Err(DriverError::Uia(format!(
                "surface '{name}' is not declared (declared: {})",
                declared.join(", ")
            )));
        };
        if slot.driver.is_none() {
            slot.driver = Some((self.factory)(name)?);
        }
        let driver = slot.driver.as_mut().expect("just ensured");
        driver.launch(
            &slot.target.command,
            &slot.target.window_name,
            self.launch_timeout,
        )?;
        self.active = Some(name.to_string());
        Ok(())
    }

    /// The active surface's name, if one has been activated.
    pub fn active_surface(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Surfaces whose first activation has launched them.
    pub fn launched_surfaces(&self) -> Vec<&str> {
        self.surfaces
            .iter()
            .filter(|(_, slot)| slot.driver.is_some())
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// The active driver — routed calls before a first activation fail
    /// CLOSED: answered from no surface, an assertion would pass against
    /// nothing.
    fn active_mut(&mut self) -> Result<&mut Box<dyn AppDriver>, DriverError> {
        let Some(name) = self.active.as_ref() else {
            return Err(DriverError::Uia(
                "no surface is active — a multi-surface run activates one per `in:` block".into(),
            ));
        };
        Ok(self
            .surfaces
            .get_mut(name)
            .and_then(|slot| slot.driver.as_mut())
            .expect("the active surface was activated"))
    }

    /// Non-`Result` queries (dialog takes) read as 'nothing fired' when
    /// no surface is active — the true answer.
    fn active_opt(&mut self) -> Option<&mut Box<dyn AppDriver>> {
        let name = self.active.clone()?;
        self.surfaces.get_mut(&name).and_then(|s| s.driver.as_mut())
    }
}

/// Writes the routing body for every listed trait method: forward to the
/// ACTIVE surface's driver, failing closed when none is active.
macro_rules! route_to_active {
    ($(fn $name:ident(&mut self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
        $(fn $name(&mut self $(, $arg: $ty)*) -> $ret {
            self.active_mut()?.$name($($arg),*)
        })*
    };
}

impl AppDriver for SurfaceRegistry {
    // Every ROUTED method is listed once; the macro writes the identical
    // body for each. A method missing from the list falls back to the
    // trait DEFAULT — the silent hole the Box impl warns about — so keep
    // it in step with that impl (the defaulted-method test guards one).
    route_to_active! {
        fn cell_hints(&mut self, selector: &UiaSelector) -> Result<Option<CellHints>, DriverError>;
        fn scope_hints(&mut self, selector: &UiaSelector) -> Result<Option<ScopeHints>, DriverError>;
        fn probe_frame(&mut self, query: &FrameQuery) -> Result<FrameProbe, DriverError>;
        fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError>;
        fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError>;
        fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError>;
        fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError>;
        fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError>;
        fn type_focused(&mut self, text: &str) -> Result<(), DriverError>;
        fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError>;
        fn drag(&mut self, from: &UiaSelector, to: &UiaSelector) -> Result<(), DriverError>;
        fn element_enabled(&mut self, selector: &UiaSelector) -> Result<bool, DriverError>;
        fn actionability_gate(&mut self, target: &UiaSelector) -> Result<Option<String>, DriverError>;
        fn element_visible(&mut self, selector: &UiaSelector) -> Result<Option<bool>, DriverError>;
        fn select_options(&mut self, selector: &UiaSelector, values: &[String]) -> Result<(), DriverError>;
        fn click_at(&mut self, selector: &UiaSelector, x_pct: f64, y_pct: f64) -> Result<(), DriverError>;
        fn surface_text(&mut self) -> Result<String, DriverError>;
        fn element_checked(&mut self, selector: &UiaSelector) -> Result<Option<bool>, DriverError>;
        fn set_checked(&mut self, selector: &UiaSelector, checked: bool) -> Result<(), DriverError>;
        fn element_attribute(&mut self, selector: &UiaSelector, name: &str) -> Result<Option<String>, DriverError>;
        fn element_computed_style(&mut self, selector: &UiaSelector, prop: &str) -> Result<String, DriverError>;
        fn scroll(&mut self, selector: Option<&UiaSelector>, to: ScrollTo) -> Result<(), DriverError>;
        fn current_url(&mut self) -> Result<String, DriverError>;
        fn probe_cookie(&mut self, name: &str) -> Result<CookieProbe, DriverError>;
        fn page_title(&mut self) -> Result<String, DriverError>;
        fn stage_session(&mut self, session: WebSession) -> Result<(), DriverError>;
        fn navigate(&mut self, url: &str) -> Result<(), DriverError>;
        fn reload(&mut self) -> Result<(), DriverError>;
        fn screen_size(&mut self) -> Result<(u32, u32), DriverError>;
        fn set_window_geometry(&mut self, width: u32, height: u32, position: Option<(i32, i32)>) -> Result<(u32, u32, i32, i32), DriverError>;
        fn debug_bundle(&mut self) -> Result<Option<DebugBundle>, DriverError>;
        fn capture(&mut self) -> Result<Option<image::RgbaImage>, DriverError>;
        fn element_rect(&mut self, selector: &UiaSelector) -> Result<Option<PixelRect>, DriverError>;
        fn password_rects(&mut self) -> Result<Vec<PixelRect>, DriverError>;
        fn scene(&mut self) -> Result<Option<String>, DriverError>;
        fn element_receives_events(&mut self, selector: &UiaSelector) -> Result<Option<bool>, DriverError>;
        fn today(&mut self) -> Result<Option<String>, DriverError>;
        fn occluding_element(&mut self, selector: &UiaSelector) -> Result<Option<String>, DriverError>;
        fn stage_mocks(&mut self, rules: Vec<WebMock>) -> Result<(), DriverError>;
        fn set_files(&mut self, selector: &UiaSelector, paths: &[String]) -> Result<(), DriverError>;
        fn context_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError>;
        fn double_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError>;
        fn hover(&mut self, selector: &UiaSelector) -> Result<(), DriverError>;
        fn arm_dialog(&mut self, arm: DialogArm) -> Result<(), DriverError>;
        fn stage_browser(&mut self, config: WebBrowserConfig) -> Result<(), DriverError>;
    }

    // Deliberately NOT routed: a caller reaching for the single-surface
    // launch got the wrong driver and is told so.
    fn launch(
        &mut self,
        _command: &str,
        _window_name: &str,
        _timeout: Duration,
    ) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "a surface registry launches per surface, at activation".into(),
        ))
    }

    fn activate_surface(&mut self, name: &str) -> Result<(), DriverError> {
        self.activate(name)
    }

    // Not `Result`-shaped, so not routable by the macro: with no active
    // surface these read as "nothing fired", which is the true answer.
    fn take_fired_dialog(&mut self) -> Option<FiredDialog> {
        self.active_opt().and_then(AppDriver::take_fired_dialog)
    }

    fn take_unexpected_dialog(&mut self) -> Option<FiredDialog> {
        self.active_opt()
            .and_then(AppDriver::take_unexpected_dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockAppDriver;

    fn target(command: &str) -> (String, AppTarget) {
        let name = if command.starts_with("http") {
            "portal"
        } else {
            "gui"
        };
        (
            name.to_string(),
            AppTarget {
                command: command.into(),
                window_name: String::new(),
            },
        )
    }

    fn registry() -> SurfaceRegistry {
        let factory: SurfaceFactory = Box::new(|name| {
            Ok(Box::new(match name {
                "gui" => MockAppDriver::new(&["OrderNo"]).with_text("OrderNo", "4711"),
                _ => MockAppDriver::new(&["Search"]).with_text("Search", "portal ready"),
            }))
        });
        SurfaceRegistry::new(
            [target("saplogon.exe"), target("https://portal.test")],
            factory,
            Duration::from_millis(10),
        )
    }

    fn sel(id: &str) -> UiaSelector {
        UiaSelector::automation_id(id)
    }

    /// Launch is lazy and routing follows activation: nothing launches at
    /// construction, each surface launches at its FIRST activation, and
    /// every routed call answers from the surface that is active NOW.
    #[test]
    fn launches_lazily_and_routes_to_the_active_surface() {
        let mut reg = registry();
        assert!(reg.launched_surfaces().is_empty(), "nothing launched yet");
        assert!(reg.active_surface().is_none());

        reg.activate("gui").expect("gui activates");
        assert_eq!(reg.launched_surfaces(), vec!["gui"], "portal still cold");
        assert_eq!(reg.read_text(&sel("OrderNo")).expect("gui"), "4711");

        reg.activate("portal").expect("portal activates");
        assert_eq!(reg.launched_surfaces(), vec!["gui", "portal"]);
        assert_eq!(
            reg.read_text(&sel("Search")).expect("portal"),
            "portal ready"
        );
        // The gui element is not on the portal: the registry answers from
        // the ACTIVE surface, never from whichever surface could answer.
        assert!(!reg.element_exists(&sel("OrderNo")).expect("asks portal"));
    }

    /// A return visit resumes the SAME driver — the factory runs once per
    /// surface, so the session (login, page, on-screen state) survives the
    /// blocks spent elsewhere. That is what "kept alive" means.
    #[test]
    fn returning_to_a_surface_resumes_the_same_driver() {
        let builds = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let log = builds.clone();
        let factory: SurfaceFactory = Box::new(move |name| {
            log.borrow_mut().push(name.to_string());
            Ok(Box::new(MockAppDriver::new(&["Field"])))
        });
        let mut reg = SurfaceRegistry::new(
            [target("saplogon.exe"), target("https://portal.test")],
            factory,
            Duration::from_millis(10),
        );
        reg.activate("gui").expect("gui activates");
        reg.activate("portal").expect("portal activates");
        reg.activate("gui").expect("gui re-activates");
        assert_eq!(
            *builds.borrow(),
            vec!["gui".to_string(), "portal".to_string()],
            "the return visit built NO new driver: same driver, same session"
        );
        assert_eq!(reg.active_surface(), Some("gui"));
    }

    /// Every routed call before a first activation fails CLOSED — an
    /// assertion answered from no surface would be a pass against nothing.
    #[test]
    fn calls_before_activation_and_unknown_surfaces_fail_closed() {
        let mut reg = registry();
        let err = reg
            .read_text(&sel("OrderNo"))
            .expect_err("no active surface");
        assert!(
            err.to_string().contains("no surface is active"),
            "says why: {err}"
        );
        let err = reg.activate("mainframe").expect_err("undeclared");
        assert!(
            err.to_string().contains("mainframe") && err.to_string().contains("gui, portal"),
            "names the stranger and the declared: {err}"
        );
        let err = reg
            .launch("calc.exe", "Calculator", Duration::from_millis(1))
            .expect_err("registry does not launch");
        assert!(err.to_string().contains("activation"), "{err}");
    }

    /// The `activate_surface` trait hook is the recorder's door in: on the
    /// registry it activates; on any single-surface driver the DEFAULT
    /// refuses by name, so a mis-wired run can never silently keep driving
    /// the wrong app.
    #[test]
    fn the_trait_hook_activates_on_the_registry_and_refuses_elsewhere() {
        let mut reg = registry();
        AppDriver::activate_surface(&mut reg, "gui").expect("hook activates");
        assert_eq!(reg.active_surface(), Some("gui"));
        let err = MockAppDriver::new(&[])
            .activate_surface("portal")
            .expect_err("single-surface driver refuses");
        assert!(err.to_string().contains("one surface"), "{err}");
    }

    /// Routing must beat the trait DEFAULT for defaulted methods too: a
    /// method missing from the macro list silently answers the default
    /// (`debug_bundle` -> None) instead of the active driver — this is the
    /// regression that a signature-shape change can quietly cause.
    #[test]
    fn defaulted_trait_methods_route_to_the_active_driver_not_the_default() {
        let factory: SurfaceFactory = Box::new(|_| {
            let mut mock = MockAppDriver::new(&[]);
            mock.debug = Some(crate::DebugBundle {
                dom_html: Some("<p>from the active surface</p>".into()),
                console: vec![],
            });
            Ok(Box::new(mock))
        });
        let mut reg = SurfaceRegistry::new(
            [(
                "gui".to_string(),
                AppTarget {
                    command: "x".into(),
                    window_name: String::new(),
                },
            )],
            factory,
            Duration::from_millis(10),
        );
        reg.activate("gui").expect("activates");
        let bundle = reg
            .debug_bundle()
            .expect("routes")
            .expect("driver answered");
        assert!(bundle
            .dom_html
            .unwrap_or_default()
            .contains("active surface"));
    }
}
