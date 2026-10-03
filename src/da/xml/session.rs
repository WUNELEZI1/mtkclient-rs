//! XML (V6) DA 会话门面
//!
//! 把 [`cmd`] 的命令构造与 [`flash`] 的收发编排，收敛成一个可直接驱动设备
//! 的会话对象 [`XmlSession`]。CLI 侧只需 `XmlSession::new(&mut *device)`，
//! 再调用语义化方法（`initialize` / `read_partition` / `reboot` ...）。
//!
//! 与 [`crate::da::xflash`]（XFlash 二进制协议，老平台）并列：本会话面向
//! MT6789+ 的 XML DA，二者通过 CLI 的 `xml` 子命令 / `--via xml` 显式选择，
//! 不自动互转，避免在未验证平台上误走协议。

use std::io::{Read, Write};

use crate::da::xml::cmd::{self, FileSystemOp, XmlCmdLifetime};
use crate::da::xml::flash;
use crate::da::xml::protocol::{XmlProtoError, XmlProtocol};
use crate::preloader::transport::BromTransport;

/// XML DA 会话：持有一条已打开的传输端口
pub struct XmlSession<'a> {
    xml: XmlProtocol<'a>,
}

impl<'a> XmlSession<'a> {
    /// 绑定到一条已打开的传输端口
    pub fn new(port: &'a mut dyn BromTransport) -> Self {
        XmlSession {
            xml: XmlProtocol::new(port),
        }
    }

    /// 发送一条命令并读取 ACK（设备拒绝时返回分类错误）
    fn send(&mut self, xml_cmd: &str) -> Result<(), XmlProtoError> {
        self.xml.send_cmd(xml_cmd)?;
        Ok(())
    }

    /// 发送命令 → 读取 `CMD:UPLOAD-FILE` 文本结果 → 收 `CMD:END`
    ///
    /// 用于 `GET-HW-INFO` / `GET-SYS-PROPERTY` / `READ-EFUSE` 等"设备回传数据"类命令。
    fn send_and_upload(&mut self, xml_cmd: &str) -> Result<String, XmlProtoError> {
        self.send(xml_cmd)?;
        let resp = self.xml.get_upload_file_resp()?;
        self.xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;
        Ok(resp)
    }

    /// 发送命令 → 接收 `CMD:DOWNLOAD-FILE` 数据流 → 收 `CMD:END`
    ///
    /// 用于 `WRITE-EFUSE` / `SECURITY-SET-*` 等"主机发送数据"类命令。
    fn send_and_download(&mut self, xml_cmd: &str, data: &[u8]) -> Result<usize, XmlProtoError> {
        self.send(xml_cmd)?;
        // DA 先询问目标文件大小，再走 DOWNLOAD-FILE
        self.xml
            .file_system_op(FileSystemOp::FileSize(data.len()))?;
        let mut noop = |_: usize, _: usize| {};
        self.xml.progress_report(&mut noop)?;
        let mut reader = data;
        let sent = self.xml.download_data(data.len(), &mut reader, &mut noop)?;
        self.xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;
        Ok(sent)
    }

    /// 会话初始化：上报主机标识 → 能力集 → 运行参数 → 通知初始化硬件
    ///
    /// 对齐 penumbra 的 XML DA 启动序列，须在任何分区/安全操作之前调用一次。
    pub fn initialize(
        &mut self,
        host_info: &str,
        log_level: &str,
        log_channel: &str,
        system_os: &str,
    ) -> Result<(), XmlProtoError> {
        self.send(&cmd::set_host_info(host_info))?;
        self.send(&cmd::host_supported_commands())?;
        self.send(&cmd::set_runtime_parameter(
            log_level,
            log_channel,
            system_os,
        ))?;
        self.send(&cmd::notify_init_hw())?;
        Ok(())
    }

    /// `BOOT-TO`：跳转到指定地址执行（`source_file` 固定 `MEM://0x0:0x0`）
    pub fn boot_to(&mut self, at_addr: u64, jmp_addr: u64) -> Result<(), XmlProtoError> {
        self.send(&cmd::boot_to(at_addr, jmp_addr))
    }

    /// `GET-HW-INFO`：读取存储/硬件信息（文本）
    pub fn get_hw_info(&mut self) -> Result<String, XmlProtoError> {
        self.send_and_upload(&cmd::get_hw_info())
    }

    /// `GET-SYS-PROPERTY`：读取设备系统属性（文本）
    pub fn get_sys_property(&mut self, key: &str) -> Result<String, XmlProtoError> {
        self.send_and_upload(&cmd::get_sys_property(key))
    }

    /// `READ-EFUSE`：读取 efuse（文本）
    pub fn read_efuse(&mut self) -> Result<String, XmlProtoError> {
        self.send_and_upload(&cmd::read_efuse())
    }

    /// `WRITE-EFUSE`：写入 efuse 数据
    pub fn write_efuse(&mut self, data: &[u8]) -> Result<usize, XmlProtoError> {
        self.send_and_download(&cmd::write_efuse(), data)
    }

    /// `SECURITY-GET-DEV-FW-INFO`：读取设备固件安全信息（文本）
    pub fn security_get_dev_fw_info(&mut self) -> Result<String, XmlProtoError> {
        self.send_and_upload(&cmd::security_get_dev_fw_info())
    }

    /// `SECURITY-SET-FLASH-POLICY`：下发 flash policy
    pub fn security_set_flash_policy(&mut self, data: &[u8]) -> Result<usize, XmlProtoError> {
        self.send_and_download(&cmd::security_set_flash_policy("MEM://0x0:0x200000"), data)
    }

    /// `SECURITY-SET-ALLINONE-SIGNATURE`：下发 all-in-one 签名
    pub fn security_set_allinone_signature(&mut self, data: &[u8]) -> Result<usize, XmlProtoError> {
        self.send_and_download(
            &cmd::security_set_allinone_signature("MEM://0x0:0x200000"),
            data,
        )
    }

    /// `FLASH-UPDATE`：进入 scatter 刷写流程（后续由 DA 主导）
    pub fn flash_update(&mut self) -> Result<(), XmlProtoError> {
        self.send(&cmd::flash_update())
    }

    /// `READ-FLASH`：按地址/长度读取
    pub fn read_flash<W, F>(
        &mut self,
        section: &str,
        offset: u64,
        size: usize,
        writer: &mut W,
        progress: &mut F,
    ) -> Result<usize, XmlProtoError>
    where
        W: Write,
        F: FnMut(usize, usize),
    {
        flash::read_flash(&mut self.xml, section, offset, size, writer, progress)
    }

    /// `WRITE-FLASH`：按地址/长度写入
    pub fn write_flash<R, F>(
        &mut self,
        section: &str,
        offset: u64,
        size: usize,
        reader: &mut R,
        progress: &mut F,
    ) -> Result<usize, XmlProtoError>
    where
        R: Read,
        F: FnMut(usize, usize),
    {
        flash::write_flash(&mut self.xml, section, offset, size, reader, progress)
    }

    /// `ERASE-FLASH`：按地址/长度擦除
    pub fn erase_flash<F>(
        &mut self,
        section: &str,
        offset: u64,
        size: usize,
        progress: &mut F,
    ) -> Result<(), XmlProtoError>
    where
        F: FnMut(usize, usize),
    {
        flash::erase_flash(&mut self.xml, section, offset, size, progress)
    }

    /// `READ-PARTITION`：整分区回读
    pub fn read_partition<W, F>(
        &mut self,
        part_name: &str,
        writer: &mut W,
        progress: &mut F,
    ) -> Result<usize, XmlProtoError>
    where
        W: Write,
        F: FnMut(usize, usize),
    {
        flash::read_partition(&mut self.xml, part_name, writer, progress)
    }

    /// `WRITE-PARTITION`：整分区写入
    pub fn write_partition<R, F>(
        &mut self,
        part_name: &str,
        size: usize,
        reader: &mut R,
        progress: &mut F,
    ) -> Result<(), XmlProtoError>
    where
        R: Read,
        F: FnMut(usize, usize),
    {
        flash::write_partition(&mut self.xml, part_name, size, reader, progress)
    }

    /// `ERASE-PARTITION`：格式化整个分区
    pub fn format_partition<F>(
        &mut self,
        part_name: &str,
        progress: &mut F,
    ) -> Result<(), XmlProtoError>
    where
        F: FnMut(usize, usize),
    {
        flash::format_partition(&mut self.xml, part_name, progress)
    }

    /// `REBOOT`：`disconnect=true` 走 `DISCONNECT`，否则 `IMMEDIATE`
    pub fn reboot(&mut self, disconnect: bool) -> Result<(), XmlProtoError> {
        self.send(&cmd::reboot(disconnect))
    }

    /// `SET-BOOT-MODE`：设置下次启动模式（FASTBOOT / META 等）
    pub fn set_boot_mode(
        &mut self,
        mode: &str,
        connect_type: &str,
        mobile_log: &str,
        adb: &str,
    ) -> Result<(), XmlProtoError> {
        self.send(&cmd::set_boot_mode(mode, connect_type, mobile_log, adb))
    }
}
