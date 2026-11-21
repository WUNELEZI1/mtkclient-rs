//! XML (V6) DA session facade
//!
//! Collapses [`cmd`] command construction and [`flash`] I/O orchestration into a session object
//! [`XmlSession`] that drives the device directly. The CLI side only needs `XmlSession::new(&mut *device)`
//! and then calls semantic methods (`initialize` / `read_partition` / `reboot` ...).
//!
//! Parallel to [`crate::da::xflash`] (XFlash binary protocol, older platforms): this session targets
//! MT6789+ XML DA. The two are explicitly selected via the CLI `xml` subcommand / `--via xml`
//! and never auto-converted, avoiding accidental protocol use on unverified platforms.

use std::io::{Read, Write};

use crate::da::xml::cmd::{self, FileSystemOp, XmlCmdLifetime};
use crate::da::xml::flash;
use crate::da::xml::protocol::{XmlProtoError, XmlProtocol};
use crate::preloader::transport::BromTransport;

/// XML DA session: holds one open transport port
pub struct XmlSession<'a> {
    xml: XmlProtocol<'a>,
}

impl<'a> XmlSession<'a> {
    /// Bind to an already-open transport port
    pub fn new(port: &'a mut dyn BromTransport) -> Self {
        XmlSession {
            xml: XmlProtocol::new(port),
        }
    }

    /// Send a command and read the ACK (returns a classified error when the device rejects)
    fn send(&mut self, xml_cmd: &str) -> Result<(), XmlProtoError> {
        self.xml.send_cmd(xml_cmd)?;
        Ok(())
    }

    /// Send a command → read the `CMD:UPLOAD-FILE` text result → receive `CMD:END`
    ///
    /// Used by device-returned-data commands such as `GET-HW-INFO` / `GET-SYS-PROPERTY` / `READ-EFUSE`.
    fn send_and_upload(&mut self, xml_cmd: &str) -> Result<String, XmlProtoError> {
        self.send(xml_cmd)?;
        let resp = self.xml.get_upload_file_resp()?;
        self.xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;
        Ok(resp)
    }

    /// Send a command → receive the `CMD:DOWNLOAD-FILE` data stream → receive `CMD:END`
    ///
    /// Used by host-sends-data commands such as `WRITE-EFUSE` / `SECURITY-SET-*`.
    fn send_and_download(&mut self, xml_cmd: &str, data: &[u8]) -> Result<usize, XmlProtoError> {
        self.send(xml_cmd)?;
        // DA asks for the target file size first, then uses DOWNLOAD-FILE
        self.xml
            .file_system_op(FileSystemOp::FileSize(data.len()))?;
        let mut noop = |_: usize, _: usize| {};
        self.xml.progress_report(&mut noop)?;
        let mut reader = data;
        let sent = self.xml.download_data(data.len(), &mut reader, &mut noop)?;
        self.xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;
        Ok(sent)
    }

    /// Session init: report host identity → capabilities → runtime parameters → notify hardware init
    ///
    /// Matching penumbra's XML DA startup sequence; must be called once before any partition/security operation.
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

    /// `BOOT-TO`: jump to the given address (`source_file` is fixed to `MEM://0x0:0x0`)
    pub fn boot_to(&mut self, at_addr: u64, jmp_addr: u64) -> Result<(), XmlProtoError> {
        self.send(&cmd::boot_to(at_addr, jmp_addr))
    }

    /// `GET-HW-INFO`: read storage/hardware info (text)
    pub fn get_hw_info(&mut self) -> Result<String, XmlProtoError> {
        self.send_and_upload(&cmd::get_hw_info())
    }

    /// `GET-SYS-PROPERTY`: read a device system property (text)
    pub fn get_sys_property(&mut self, key: &str) -> Result<String, XmlProtoError> {
        self.send_and_upload(&cmd::get_sys_property(key))
    }

    /// `READ-EFUSE`: read efuse (text)
    pub fn read_efuse(&mut self) -> Result<String, XmlProtoError> {
        self.send_and_upload(&cmd::read_efuse())
    }

    /// `WRITE-EFUSE`: write efuse data
    pub fn write_efuse(&mut self, data: &[u8]) -> Result<usize, XmlProtoError> {
        self.send_and_download(&cmd::write_efuse(), data)
    }

    /// `SECURITY-GET-DEV-FW-INFO`: read device firmware security info (text)
    pub fn security_get_dev_fw_info(&mut self) -> Result<String, XmlProtoError> {
        self.send_and_upload(&cmd::security_get_dev_fw_info())
    }

    /// `SECURITY-SET-FLASH-POLICY`: deliver the flash policy
    pub fn security_set_flash_policy(&mut self, data: &[u8]) -> Result<usize, XmlProtoError> {
        self.send_and_download(&cmd::security_set_flash_policy("MEM://0x0:0x200000"), data)
    }

    /// `SECURITY-SET-ALLINONE-SIGNATURE`: deliver the all-in-one signature
    pub fn security_set_allinone_signature(&mut self, data: &[u8]) -> Result<usize, XmlProtoError> {
        self.send_and_download(
            &cmd::security_set_allinone_signature("MEM://0x0:0x200000"),
            data,
        )
    }

    /// `FLASH-UPDATE`: enter the scatter flashing flow (then driven by DA)
    pub fn flash_update(&mut self) -> Result<(), XmlProtoError> {
        self.send(&cmd::flash_update())
    }

    /// `READ-FLASH`: read by address/length
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

    /// `WRITE-FLASH`: write by address/length
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

    /// `ERASE-FLASH`: erase by address/length
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

    /// `READ-PARTITION`: read back a whole partition
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

    /// `WRITE-PARTITION`: write a whole partition
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

    /// `ERASE-PARTITION`: format the entire partition
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

    /// `REBOOT`: `disconnect=true` uses `DISCONNECT`, otherwise `IMMEDIATE`
    pub fn reboot(&mut self, disconnect: bool) -> Result<(), XmlProtoError> {
        self.send(&cmd::reboot(disconnect))
    }

    /// `SET-BOOT-MODE`: set the next boot mode (FASTBOOT / META etc.)
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
