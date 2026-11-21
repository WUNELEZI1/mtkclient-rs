//! Device control commands (reset / shutdown / vbmeta patch placeholder)
//!
//! See the [`crate::da::xflash::io`] module docs.

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// Reboot the device through the DA (reboot into the system).
    ///
    /// Internally calls `da_shutdown()` (bootmode=1 = HOME_SCREEN); after receiving SHUTDOWN the DA
    /// jumps to preloader → system directly (see `da_shutdown`, enablewdt=0).
    /// Note: the device must be in an idle state that accepts SHUTDOWN (not in a mid-read data stream),
    /// otherwise the DA rejects the command — callers (cmd_reboot) already block "unfinished reads".
    pub fn reset_device(&mut self) -> Result<(), String> {
        self.da_shutdown()
    }
}
