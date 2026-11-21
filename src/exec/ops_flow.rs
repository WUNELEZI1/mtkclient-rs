//! End-to-end flow tests: read / write (including resume) / erase / seccfg / vbmeta / unlock.
//!
//! Everything runs on the same virtual device as `--sim`, so what is validated is the **real-device-isomorphic
//! execution path** (Executor → Session → da::engine → device-side state machine), not mock return values.

use super::*;
use crate::ops::erase::EraseOp;
use crate::ops::format::FormatOp;
use crate::ops::printgpt::PrintGptOp;
use crate::ops::read::ReadOp;
use crate::ops::seccfg::{SeccfgAction, SeccfgOp};
use crate::ops::unlock::UnlockOp;
use crate::ops::vbmeta::VbmetaOp;
use crate::ops::write::WriteOp;
use crate::session::SimSession;
use crate::transport::vdev::VirtualDevice;

fn sim(preset: &str) -> SimSession {
    let dev = VirtualDevice::from_preset(preset).expect("预设存在");
    SimSession::new(dev).with_da(vec![0xABu8; 4096])
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mtk-ops-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn exec_yes() -> Executor {
    Executor::new(Mode::Auto).with_assume_yes(true)
}

#[cfg(test)]
#[path = "ops_flow/flow_misc.rs"]
mod flow_misc;
#[cfg(test)]
#[path = "ops_flow/flow_read.rs"]
mod flow_read;
#[cfg(test)]
#[path = "ops_flow/flow_security.rs"]
mod flow_security;
#[cfg(test)]
#[path = "ops_flow/flow_write.rs"]
mod flow_write;
