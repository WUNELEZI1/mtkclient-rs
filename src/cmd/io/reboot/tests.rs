use super::plan::{RebootAction, RebootNotice, plan_reboot};

// Expected branch for the canonical connection context (is_brom = true, is_preloader = false).
// Written directly from the original cmd_reboot semantics and used as a lookup table.
fn expected_canonical(via: &str, mode: &str, da_active: bool) -> RebootAction {
    use RebootAction::*;
    if via == "preloader" && !da_active {
        return PreloaderPattern;
    }
    // DA-active --via preloader degrades to para.
    let via = if via == "preloader" { "para" } else { via };
    match (via, mode) {
        (_, "system") => {
            if da_active {
                SystemDaShutdown
            } else {
                SystemWatchdog
            }
        }
        ("xml", "fastboot") => FastbootXml,
        ("para", "fastboot") => FastbootPara,
        (_, "fastboot") => FastbootMisc,
        ("para", "recovery") | ("para", "fastbootd") => RecoveryPara,
        ("xml", "recovery") | ("xml", "fastbootd") => RecoveryXmlFallback,
        (_, "recovery") | (_, "fastbootd") => RecoveryMisc,
        ("da", "meta") => MetaDa,
        ("xml", "meta") => MetaXml,
        ("para", "meta") => MetaPara,
        (_, "meta") => MetaMisc,
        _ => UnsupportedMode,
    }
}

#[test]
fn plan_reboot_matches_original_for_all_via_mode_da_combinations() {
    let vias = ["para", "misc", "da", "xml", "preloader"];
    let modes = ["system", "fastboot", "recovery", "fastbootd", "meta"];
    let da_states = [false, true];
    let mut count = 0usize;

    for via in vias {
        for mode in modes {
            for &da_active in &da_states {
                let plan = plan_reboot(via, mode, da_active, true, false);
                let expected = expected_canonical(via, mode, da_active);
                assert_eq!(
                    plan.action, expected,
                    "via={via} mode={mode} da_active={da_active}"
                );
                count += 1;
            }
        }
    }

    assert_eq!(count, 5 * 5 * 2);
}

#[test]
fn plan_reboot_key_combinations_are_explicit() {
    // Literal expectations that pin the original semantics independently of the lookup table.
    assert_eq!(
        plan_reboot("preloader", "fastboot", false, true, false).action,
        RebootAction::PreloaderPattern
    );
    assert_eq!(
        plan_reboot("preloader", "system", true, true, false).action,
        RebootAction::SystemDaShutdown
    );
    assert_eq!(
        plan_reboot("para", "system", false, true, false).action,
        RebootAction::SystemWatchdog
    );
    assert_eq!(
        plan_reboot("para", "system", true, true, false).action,
        RebootAction::SystemDaShutdown
    );
    assert_eq!(
        plan_reboot("xml", "fastboot", false, true, false).action,
        RebootAction::FastbootXml
    );
    assert_eq!(
        plan_reboot("da", "meta", false, true, false).action,
        RebootAction::MetaDa
    );
    assert_eq!(
        plan_reboot("xml", "meta", false, true, false).action,
        RebootAction::MetaXml
    );
    assert_eq!(
        plan_reboot("xml", "recovery", false, true, false).action,
        RebootAction::RecoveryXmlFallback
    );
    assert_eq!(
        plan_reboot("misc", "unknown", false, true, false).action,
        RebootAction::UnsupportedMode
    );
}

#[test]
fn plan_reboot_notices_and_degradations() {
    // DA-active preloader degrades to para and emits a notice carrying the mode.
    let plan = plan_reboot("preloader", "fastboot", true, true, false);
    assert_eq!(plan.via, "para");
    assert_eq!(
        plan.notices,
        vec![RebootNotice::DaDegradedPreloader {
            mode: "fastboot".to_string()
        }]
    );

    // The Preloader connection rejects non-misc/para via and degrades to para.
    let plan = plan_reboot("xml", "system", false, false, true);
    assert_eq!(plan.via, "para");
    assert_eq!(
        plan.notices,
        vec![RebootNotice::PreloaderViaUnsupported {
            via: "xml".to_string()
        }]
    );

    // misc/para are allowed in Preloader connection: no notice.
    assert!(
        plan_reboot("misc", "system", false, false, true)
            .notices
            .is_empty()
    );
    assert!(
        plan_reboot("para", "system", false, false, true)
            .notices
            .is_empty()
    );

    // Pure preloader path emits no notice and keeps via unchanged.
    let plan = plan_reboot("preloader", "meta", false, true, false);
    assert_eq!(plan.via, "preloader");
    assert!(plan.notices.is_empty());
}

#[test]
fn plan_reboot_connection_state_sensitivity() {
    // --via xml on fastboot only takes the XML path on BROM.
    assert_eq!(
        plan_reboot("xml", "fastboot", false, true, false).action,
        RebootAction::FastbootXml
    );
    assert_eq!(
        plan_reboot("xml", "fastboot", false, false, false).action,
        RebootAction::FastbootMisc
    );

    // In Preloader connection, --via da degrades to para (with a notice).
    let plan = plan_reboot("da", "meta", false, false, true);
    assert_eq!(plan.action, RebootAction::MetaPara);
    assert_eq!(
        plan.notices,
        vec![RebootNotice::PreloaderViaUnsupported {
            via: "da".to_string()
        }]
    );
}
