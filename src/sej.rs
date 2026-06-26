#![allow(dead_code)]

use crate::da_xflash::DAXFlash;
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use log::{debug, info};
use std::cell::RefCell;
use std::thread::sleep;
use std::time::Duration;

const HACC_INIT: u32 = 0x1000_A020;
const HACC_DATA: u32 = 0x1000_A010;
const HACC_CTRL: u32 = 0x1000_A008;
const HACC_OUT0: u32 = 0x1000_A050;

struct HaccBackendVtable {
    ptr: *mut (),
    reg_write32: unsafe fn(*mut (), u32, u32) -> Result<(), String>,
    reg_read32: unsafe fn(*mut (), u32) -> Result<u32, String>,
    is_hacc_ready: unsafe fn(*mut ()) -> bool,
}

thread_local! {
    static HACC_BACKEND: RefCell<Option<HaccBackendVtable>> = const { RefCell::new(None) };
}

pub trait HaccBackend {
    fn is_hacc_ready(&self) -> bool;
    fn reg_write32(&mut self, addr: u32, value: u32) -> Result<(), String>;
    fn reg_read32(&mut self, addr: u32) -> Result<u32, String>;
}

impl<'a> HaccBackend for DAXFlash<'a> {
    fn is_hacc_ready(&self) -> bool {
        self.daext
    }

    fn reg_write32(&mut self, addr: u32, value: u32) -> Result<(), String> {
        if !self.daext {
            return Err("DA Extensions 未启用，无法访问 HACC 寄存器".to_string());
        }
        let resp = self.send_devctrl(addr, Some(&value.to_le_bytes()))?;
        if !resp.is_empty() {
            debug!(
                "[HACC] write 0x{:08X} -> {:08X}, resp {:02X?}",
                addr, value, resp
            );
        }
        Ok(())
    }

    fn reg_read32(&mut self, addr: u32) -> Result<u32, String> {
        if !self.daext {
            return Err("DA Extensions 未启用，无法访问 HACC 寄存器".to_string());
        }
        let resp = self.send_devctrl(addr, None)?;
        if resp.len() < 4 {
            return Err(format!(
                "HACC read 0x{:08X} 返回数据太短: {}",
                addr,
                resp.len()
            ));
        }
        Ok(u32::from_le_bytes(resp[0..4].try_into().unwrap()))
    }
}

unsafe fn backend_write_trampoline<T: HaccBackend>(
    ptr: *mut (),
    addr: u32,
    value: u32,
) -> Result<(), String> {
    let backend = unsafe { &mut *(ptr as *mut T) };
    backend.reg_write32(addr, value)
}

unsafe fn backend_read_trampoline<T: HaccBackend>(ptr: *mut (), addr: u32) -> Result<u32, String> {
    let backend = unsafe { &mut *(ptr as *mut T) };
    backend.reg_read32(addr)
}

unsafe fn backend_ready_trampoline<T: HaccBackend>(ptr: *mut ()) -> bool {
    let backend = unsafe { &*(ptr as *mut T) };
    backend.is_hacc_ready()
}

/// 在当前线程内临时绑定 HACC 后端，供 `sej_hacc_sign` 使用。
pub fn with_backend<T, R>(backend: &mut T, f: impl FnOnce() -> R) -> R
where
    T: HaccBackend,
{
    let vtable = HaccBackendVtable {
        ptr: backend as *mut T as *mut (),
        reg_write32: backend_write_trampoline::<T>,
        reg_read32: backend_read_trampoline::<T>,
        is_hacc_ready: backend_ready_trampoline::<T>,
    };
    HACC_BACKEND.with(|slot| {
        let previous = slot.replace(Some(vtable));
        let result = f();
        slot.replace(previous);
        result
    })
}

fn with_backend_mut<R>(
    f: impl FnOnce(&HaccBackendVtable) -> Result<R, String>,
) -> Result<R, String> {
    HACC_BACKEND.with(|slot| {
        let vtable = slot.borrow();
        let ctx = vtable
            .as_ref()
            .ok_or_else(|| "HACC backend unavailable: 请先绑定 DA Extensions 后端".to_string())?;
        f(ctx)
    })
}

fn wait_hacc_ready(ctx: &HaccBackendVtable) -> Result<(), String> {
    for _ in 0..50 {
        let status = unsafe { (ctx.reg_read32)(ctx.ptr, HACC_CTRL)? };
        if status == 0x8000 {
            return Ok(());
        }
        sleep(Duration::from_millis(10));
    }
    Err("HACC 超时等待状态 0x8000".to_string())
}

const SEJ_SW_KEY: &[u8; 32] = b"25A1763A21BC854CD569DC23B4782B63";
const SEJ_IV: &[u8; 16] = &[
    0x57, 0x32, 0x5A, 0x5A, 0x12, 0x54, 0x97, 0x66, 0x12, 0x54, 0x97, 0x66, 0x57, 0x32, 0x5A, 0x5A,
];

const CUSTOM_SEED_PREFIX: [u8; 4] = [0x00, 0xBE, 0x13, 0xBB];
const SEJ_HW_KEY: [u8; 16] = [0u8; 16];
const G_HACC_CFG_1: [u32; 8] = [
    0x9ED40400, 0x00E884A1, 0xE3F083BD, 0x2F4E6D8A, 0xFF838E5C, 0xE940A0E3, 0x8D4DECC6, 0x45FC0989,
];

#[allow(dead_code)] // 预留：HACC 签名自定义 seed/IV 生成，用于安全启动绕过
fn generate_custom_seed_iv() -> [u8; 16] {
    let seed = u32::from_le_bytes(CUSTOM_SEED_PREFIX);
    let rot = seed.rotate_left(16);
    let iv_parts: [u32; 4] = [seed, (!seed).wrapping_add(1), rot, (!rot).wrapping_add(1)];
    let mut iv = [0u8; 16];
    for (i, part) in iv_parts.iter().enumerate() {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&part.to_le_bytes());
    }
    iv
}

fn xor_g_hacc_cfg_1(data: &mut [u8]) {
    for i in 0..data.len().min(16) {
        let cfg_word = G_HACC_CFG_1[i / 4];
        data[i] ^= (cfg_word >> ((i % 4) * 8)) as u8;
    }
}

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

fn sw_key_default() -> [u8; 32] {
    let mut key = [0u8; 32];
    key.copy_from_slice(SEJ_SW_KEY);
    key
}

fn extract_sw_key_from_preloader(preloader_data: &[u8]) -> Option<[u8; 32]> {
    let pattern = [0x4D, 0x4D, 0x4D, 0x01, 0x30];
    let idx = preloader_data
        .windows(pattern.len())
        .position(|w| w == pattern)?;
    let start = idx + 0x0C;
    let end = start + 32;
    if end > preloader_data.len() {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&preloader_data[start..end]);
    Some(key)
}

fn sej_sec_cfg_sw_encrypt_with_key(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let cipher = Aes256CbcEnc::new_from_slices(key, SEJ_IV)
        .map_err(|e| format!("AES-256-CBC 加密初始化失败: {:?}", e))?;
    let mut buf = data.to_vec();
    while !buf.len().is_multiple_of(16) {
        buf.push(0);
    }
    let len = buf.len();
    cipher
        .encrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf, len)
        .map_err(|e| format!("AES-256-CBC 加密失败: {:?}", e))?;
    Ok(buf)
}

fn sej_sec_cfg_sw_decrypt_with_key(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let cipher = Aes256CbcDec::new_from_slices(key, SEJ_IV)
        .map_err(|e| format!("AES-256-CBC 解密初始化失败: {:?}", e))?;
    let mut buf = data.to_vec();
    if !buf.len().is_multiple_of(16) {
        return Err(format!(
            "sej_sec_cfg_sw 数据长度不是 16 的倍数: {}",
            buf.len()
        ));
    }
    cipher
        .decrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| format!("AES-256-CBC 解密失败: {:?}", e))?;
    Ok(buf)
}

/// 软件模式 AES-256-CBC 解密（sej_sec_cfg_sw 解密）
/// 对齐 Python: sej_sec_cfg_sw(data, encrypt=False)
pub(crate) fn sej_sec_cfg_sw_decrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    sej_sec_cfg_sw_decrypt_with_key(data, &sw_key_default())
}

/// 软件模式 AES-256-CBC 加密（sej_sec_cfg_sw 加密）
/// 对齐 Python: sej_sec_cfg_sw(data, encrypt=True)
pub(crate) fn sej_sec_cfg_sw_encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    sej_sec_cfg_sw_encrypt_with_key(data, &sw_key_default())
}

/// 硬件模式 AES-128-CBC 加密（SEJ V3/V4 hwtype 签名用）
/// 对齐 Python: sej_sec_cfg_hw_V3 → hw_aes128_cbc_encrypt
/// 由于无法访问硬件 SEJ 寄存器，此处用软件模拟
/// 使用 g_HACC_CFG_1 作为 IV，AES-128-CBC
pub(crate) fn sej_sec_cfg_hw_v3_encrypt(data: &[u8], legacy: bool) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let mut iv = [0u8; 16];
    for i in 0..4 {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
    }

    let _ = legacy;
    let cipher = Aes128CbcEnc::new(&SEJ_HW_KEY.into(), &iv.into());
    let mut buf = data.to_vec();
    while !buf.len().is_multiple_of(16) {
        buf.push(0);
    }
    let buf_len = buf.len();
    cipher
        .encrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf, buf_len)
        .map_err(|e| format!("AES-128-CBC 加密失败: {:?}", e))?;
    Ok(buf)
}

/// 硬件模式 V2 加密（sej_sec_cfg_hw）
/// 对齐 Python: sej_sec_cfg_hw
pub(crate) fn sej_sec_cfg_hw_encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let mut iv = [0u8; 16];
    for i in 0..4 {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
    }
    let cipher = Aes128CbcEnc::new(&SEJ_HW_KEY.into(), &iv.into());
    let mut buf = data.to_vec();
    xor_g_hacc_cfg_1(&mut buf);
    while !buf.len().is_multiple_of(16) {
        buf.push(0);
    }
    let buf_len = buf.len();
    cipher
        .encrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf, buf_len)
        .map_err(|e| format!("AES-128-CBC 加密 (V2) 失败: {:?}", e))?;
    Ok(buf)
}

/// 硬件模式 AES-128-CBC 解密（V3/V4 用）
pub(crate) fn sej_sec_cfg_hw_v3_decrypt(data: &[u8], legacy: bool) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcDec = cbc::Decryptor<Aes128>;

    let mut iv = [0u8; 16];
    for i in 0..4 {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
    }

    let _ = legacy;
    let cipher = Aes128CbcDec::new(&SEJ_HW_KEY.into(), &iv.into());
    let mut buf = data.to_vec();
    if !buf.len().is_multiple_of(16) {
        return Err("V3 解密数据长度不是 16 的倍数".to_string());
    }
    cipher
        .decrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| format!("AES-128-CBC 解密失败: {:?}", e))?;
    Ok(buf)
}

/// 硬件模式 V2 解密
pub(crate) fn sej_sec_cfg_hw_decrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcDec = cbc::Decryptor<Aes128>;

    let mut iv = [0u8; 16];
    for i in 0..4 {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
    }
    let cipher = Aes128CbcDec::new(&SEJ_HW_KEY.into(), &iv.into());
    let mut buf = data.to_vec();
    if !buf.len().is_multiple_of(16) {
        return Err("V2 解密数据长度不是 16 的倍数".to_string());
    }
    cipher
        .decrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| format!("AES-128-CBC 解密 (V2) 失败: {:?}", e))?;
    xor_g_hacc_cfg_1(&mut buf);
    Ok(buf)
}

fn extract_hacc_output(ctx: &HaccBackendVtable, data: &[u8]) -> Result<[u8; 32], String> {
    let mut input = [0u8; 32];
    let used_len = data.len().min(32);
    input[..used_len].copy_from_slice(&data[..used_len]);
    let mut output = [0u8; 32];

    info!("HACC init");
    unsafe { (ctx.reg_write32)(ctx.ptr, HACC_INIT, 0) }?;

    for (block_idx, block) in input.chunks(16).enumerate() {
        let mut chunk = [0u8; 16];
        chunk[..block.len()].copy_from_slice(block);

        let w0 = u32::from_le_bytes(chunk[0..4].try_into().unwrap());
        let w1 = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
        let w2 = u32::from_le_bytes(chunk[8..12].try_into().unwrap());
        let w3 = u32::from_le_bytes(chunk[12..16].try_into().unwrap());

        info!("HACC run");
        debug!(
            "[HACC] block {} write data: {:08X} {:08X} {:08X} {:08X}",
            block_idx, w0, w1, w2, w3
        );
        unsafe { (ctx.reg_write32)(ctx.ptr, HACC_DATA, w0) }?;
        unsafe { (ctx.reg_write32)(ctx.ptr, HACC_DATA + 4, w1) }?;
        unsafe { (ctx.reg_write32)(ctx.ptr, HACC_DATA + 8, w2) }?;
        unsafe { (ctx.reg_write32)(ctx.ptr, HACC_DATA + 12, w3) }?;
        unsafe { (ctx.reg_write32)(ctx.ptr, HACC_CTRL, 1) }?;
        wait_hacc_ready(ctx)?;

        let r0 = unsafe { (ctx.reg_read32)(ctx.ptr, HACC_OUT0) }?;
        let r1 = unsafe { (ctx.reg_read32)(ctx.ptr, HACC_OUT0 + 4) }?;
        let r2 = unsafe { (ctx.reg_read32)(ctx.ptr, HACC_OUT0 + 8) }?;
        let r3 = unsafe { (ctx.reg_read32)(ctx.ptr, HACC_OUT0 + 12) }?;

        output[block_idx * 16..block_idx * 16 + 4].copy_from_slice(&r0.to_le_bytes());
        output[block_idx * 16 + 4..block_idx * 16 + 8].copy_from_slice(&r1.to_le_bytes());
        output[block_idx * 16 + 8..block_idx * 16 + 12].copy_from_slice(&r2.to_le_bytes());
        output[block_idx * 16 + 12..block_idx * 16 + 16].copy_from_slice(&r3.to_le_bytes());
    }

    info!("HACC terminate");
    unsafe { (ctx.reg_write32)(ctx.ptr, HACC_CTRL, 2) }?;
    unsafe { (ctx.reg_write32)(ctx.ptr, HACC_INIT, 0) }?;
    Ok(output)
}

/// 通过 HACC 引擎对输入做签名/加密，并返回 32 字节结果。
/// 如果 DA Extensions 后端可用，优先走硬件；否则在提供 preloader 数据时走软件路径。
pub fn sej_hacc_sign(
    data: &[u8],
    hw_code: u16,
    preloader_data: Option<&[u8]>,
) -> Result<[u8; 32], String> {
    let mut sw_key = sw_key_default();
    if let Some(preloader) = preloader_data
        && let Some(key) = extract_sw_key_from_preloader(preloader)
    {
        sw_key = key;
    }

    match with_backend_mut(|ctx| extract_hacc_output(ctx, data)) {
        Ok(sig) => {
            debug!("[SEJ] hw_sign success, hw_code=0x{:04X}", hw_code);
            Ok(sig)
        }
        Err(e) => {
            debug!(
                "[SEJ] hw_sign unavailable (hw_code=0x{:04X}): {}, fallback to software path",
                hw_code, e
            );
            let enc = sej_sec_cfg_sw_encrypt_with_key(data, &sw_key)?;
            let mut out = [0u8; 32];
            out.copy_from_slice(&enc[..32.min(enc.len())]);
            Ok(out)
        }
    }
}
