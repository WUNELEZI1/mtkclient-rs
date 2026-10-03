//! 设备控制命令（复位 / 关闭 / vbmeta 修补占位）
//!
//! 见 [`crate::da::xflash::io`] 模块文档。

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// 通过 DA 重启设备（reboot 到系统）。
    ///
    /// 内部调用 `da_shutdown()`（bootmode=1 = HOME_SCREEN）；重启由 DA 收到
    /// SHUTDOWN 后直接跳转到 preloader → system 完成（详见 `da_shutdown`，enablewdt=0）。
    /// 注意：设备须处于可下发 SHUTDOWN 的 idle 状态（不在 mid-read 数据流态），
    /// 否则 DA 会拒绝该命令 —— 调用方（cmd_reboot）已对“未完成的读取”做拦截。
    pub fn reset_device(&mut self) -> Result<(), String> {
        self.da_shutdown()
    }
}
