/// The concrete reboot strategy chosen for a given (via, mode, DA-state, connection-state) input.
///
/// Every variant corresponds to one of the original I/O branches in `cmd_reboot`; executing a
/// plan means running exactly the same calls and emitting exactly the same messages as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RebootAction {
    /// `--via preloader` with no active DA: run the pure-Preloader protocol flow.
    PreloaderPattern,
    /// System mode with an active DA: DA SHUTDOWN(bootmode=HOME_SCREEN) then close the port.
    SystemDaShutdown,
    /// System mode without an active DA: raw watchdog hard reset (fallback).
    SystemWatchdog,
    /// Fastboot with `--via xml` on BROM: XML DA direct reboot to fastboot.
    FastbootXml,
    /// Fastboot with `--via para`: write para then DA reboot.
    FastbootPara,
    /// Fastboot with any other via: write misc then DA reboot.
    FastbootMisc,
    /// Recovery/fastbootd with `--via para`: write para then DA reboot.
    RecoveryPara,
    /// Recovery/fastbootd with `--via da` (or any via other than xml/para): write misc then DA reboot.
    RecoveryMisc,
    /// Recovery/fastbootd with `--via xml`: warn, fall back to misc, then DA reboot.
    RecoveryXmlFallback,
    /// Meta with `--via da`: DA SET_META_BOOT_MODE then DA reboot.
    MetaDa,
    /// Meta with `--via xml`: XML DA direct reboot to meta.
    MetaXml,
    /// Meta with `--via para`: write para then DA reboot.
    MetaPara,
    /// Meta with any other via: write misc then DA reboot.
    MetaMisc,
    /// Unknown mode: report the original error.
    UnsupportedMode,
}

/// A user-visible notice that must be emitted before a plan is executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RebootNotice {
    /// DA is active, so `--via preloader` degrades to para; carries the mode for the message.
    DaDegradedPreloader { mode: String },
    /// The Preloader connection rejects the requested via; carries the rejected via for the message.
    PreloaderViaUnsupported { via: String },
}

/// Pure result of planning a reboot: which action to run, the effective via, and any notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RebootPlan {
    pub(crate) action: RebootAction,
    /// The `--via` value after any degradation (para/misc/da/xml/preloader).
    pub(crate) via: String,
    /// Notices to emit before executing the action, in order.
    pub(crate) notices: Vec<RebootNotice>,
}

/// Purely map `(via, mode, da_active, is_brom, is_preloader)` to a reboot strategy.
///
/// No I/O and no logging happen here: messages are carried in `RebootPlan::notices` or emitted by
/// `cmd_reboot` while executing the chosen action. All branch semantics and message text match the
/// original implementation exactly.
pub(crate) fn plan_reboot(
    via: &str,
    mode: &str,
    da_active: bool,
    is_brom: bool,
    is_preloader: bool,
) -> RebootPlan {
    let mut notices = Vec::new();
    let mut effective_via = via;

    if via == "preloader" {
        if da_active {
            // The Pattern protocol needs the original handshake, which is gone in DA mode.
            notices.push(RebootNotice::DaDegradedPreloader {
                mode: mode.to_string(),
            });
            effective_via = "para";
        } else {
            return RebootPlan {
                action: RebootAction::PreloaderPattern,
                via: effective_via.to_string(),
                notices,
            };
        }
    }

    // In Preloader connection only misc/para are supported; degrade anything else to para.
    if is_preloader && !matches!(effective_via, "misc" | "para") {
        notices.push(RebootNotice::PreloaderViaUnsupported {
            via: effective_via.to_string(),
        });
        effective_via = "para";
    }

    let action = match mode {
        "system" => {
            if da_active {
                RebootAction::SystemDaShutdown
            } else {
                RebootAction::SystemWatchdog
            }
        }
        "fastboot" => {
            if is_brom && effective_via == "xml" {
                RebootAction::FastbootXml
            } else if effective_via == "para" {
                RebootAction::FastbootPara
            } else {
                RebootAction::FastbootMisc
            }
        }
        "recovery" | "fastbootd" => match effective_via {
            "para" => RebootAction::RecoveryPara,
            "xml" => RebootAction::RecoveryXmlFallback,
            _ => RebootAction::RecoveryMisc,
        },
        "meta" => match effective_via {
            "da" => RebootAction::MetaDa,
            "xml" => RebootAction::MetaXml,
            "para" => RebootAction::MetaPara,
            _ => RebootAction::MetaMisc,
        },
        _ => RebootAction::UnsupportedMode,
    };

    RebootPlan {
        action,
        via: effective_via.to_string(),
        notices,
    }
}
