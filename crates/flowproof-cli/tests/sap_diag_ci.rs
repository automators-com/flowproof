//! Throwaway diagnostic: dump the actual screen content after navigating to
//! VA01 on the CI runner itself, to see directly what's really there instead
//! of guessing from error messages. Not part of the suite - delete after use.
#![cfg(windows)]

use flowproof_adapters::sap_com::SapAppDriver;
use flowproof_driver::{AppDriver, UiaSelector};

#[test]
fn dump_va01_on_runner() {
    if std::env::var("FLOWPROOF_E2E_SAP").as_deref() != Ok("1") {
        eprintln!("skipping: set FLOWPROOF_E2E_SAP=1");
        return;
    }
    let mut driver = SapAppDriver::new().expect("COM engine");
    driver
        .launch("", "", std::time::Duration::from_secs(30))
        .expect("attach");
    driver.navigate("/nVA01").expect("navigate");
    std::thread::sleep(std::time::Duration::from_secs(5));

    eprintln!("=== SURFACE AFTER /nVA01, 5s later ===");
    eprintln!("{}", driver.surface_text().expect("surface"));
    eprintln!("=== END SURFACE ===");

    let exists = driver
        .element_exists(&UiaSelector::automation_id("wnd[0]/usr/ctxtVBAK-AUART"))
        .unwrap_or(false);
    eprintln!("DIAG: ctxtVBAK-AUART exists = {exists}");

    eprintln!("=== SCENE (interactable elements, id tokens) ===");
    eprintln!("{}", driver.scene().unwrap_or(None).unwrap_or_default());
    eprintln!("=== END SCENE ===");
}
