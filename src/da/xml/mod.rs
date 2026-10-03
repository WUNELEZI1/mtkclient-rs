//! XML (V6) DA 协议
//!
//! 面向 MT6789 及更新平台：老平台用 XFlash（[`crate::da::xflash`]），新平台
//! DA 走 XML 报文。本模块对齐 penumbra `core/src/da/xml/`：
//!
//! - [`cmd`]      — XML 命令构造（纯函数，无 I/O）
//! - [`protocol`] — 帧编解码 / ACK / 生命周期 / 数据收发 会话层
//! - [`flash`]    — 基于 XML 协议的分区读 / 写 / 擦 / 格式化
//! - [`session`]  — 面向调用方的高层会话门面（把上述原语编排成完整操作）

pub mod cmd;
pub mod flash;
pub mod protocol;
pub mod session;

pub use session::XmlSession;
