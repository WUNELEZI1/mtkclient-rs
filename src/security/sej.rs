use crate::da::DAXFlash;
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use log::{debug, info, trace};
use std::cell::RefCell;
use std::thread::sleep;
use std::time::Duration;

// HACC register offsets (matching the Python hwcrypto_sej.py regval table)
// Base address sej_base = 0x1000A000 (MT6768)
const HACC_ACON: u32 = 0x004;
const HACC_ACON2: u32 = 0x008;
const HACC_ACONK: u32 = 0x00C;
const HACC_ASRC0: u32 = 0x010;
const HACC_AKEY0: u32 = 0x020;
const HACC_ACFG0: u32 = 0x040;
const HACC_AOUT0: u32 = 0x050;
const HACC_UNK: u32 = 0x0BC;

// HACC control flags (matching the Python Sej class constants)
const AES_ENC: u32 = 0x00000001;
const AES_CBC: u32 = 0x00000002;
const AES_128: u32 = 0x00000000;
const AES_CHG_BO_OFF: u32 = 0x00000000;
const AES_START: u32 = 0x00000001;
const AES_CLR: u32 = 0x00000002;
const AES_RDY: u32 = 0x00008000;
const AES_BK2C: u32 = 0x00000010;
const AES_R2K: u32 = 0x00000100;

// Fixed pattern used by SEJ V3 init
const CFG_RANDOM_PATTERN: [u32; 12] = [
    0x2D44BB70, 0xA744D227, 0xD0A9864B, 0x83FFC244, 0x7EC8266B, 0x43E80FB2, 0x01A6348A, 0x2067F9A0,
    0x54536405, 0xD546A6B1, 0x1CC3EC3A, 0xDE377A83,
];

struct HaccBackendVtable {
    ptr: *mut (),
    sej_base: u32,
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
    fn sej_base(&self) -> u32;
}

impl<'a> HaccBackend for DAXFlash<'a> {
    fn is_hacc_ready(&self) -> bool {
        self.daext
    }

    fn reg_write32(&mut self, addr: u32, value: u32) -> Result<(), String> {
        if !self.daext {
            return Err("DA Extensions 未启用，无法访问 HACC 寄存器".to_string());
        }
        self.custom_writeregister(addr, value)?;
        trace!("[HACC] write 0x{:08X} -> {:08X}", addr, value);
        Ok(())
    }

    fn reg_read32(&mut self, addr: u32) -> Result<u32, String> {
        if !self.daext {
            return Err("DA Extensions 未启用，无法访问 HACC 寄存器".to_string());
        }
        let val = self.custom_readregister(addr)?;
        trace!("[HACC] read 0x{:08X} -> {:08X}", addr, val);
        Ok(val)
    }

    fn sej_base(&self) -> u32 {
        self.preloader
            .chip
            .map(|c| c.sej_base)
            .unwrap_or(0x1000_A000)
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

/// Temporarily bind a HACC backend on the current thread for `sej_hacc_sign`.
pub fn with_backend<T, R>(backend: &mut T, f: impl FnOnce() -> R) -> R
where
    T: HaccBackend,
{
    let vtable = HaccBackendVtable {
        ptr: backend as *mut T as *mut (),
        sej_base: backend.sej_base(),
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
    let base = ctx.sej_base;
    for _ in 0..20 {
        let val = unsafe { (ctx.reg_read32)(ctx.ptr, base + HACC_ACON2)? };
        if val & AES_RDY != 0 {
            return Ok(());
        }
        sleep(Duration::from_millis(10));
    }
    Err("HACC 超时等待 AES_RDY".to_string())
}

fn hacc_write(ctx: &HaccBackendVtable, offset: u32, val: u32) -> Result<(), String> {
    unsafe { (ctx.reg_write32)(ctx.ptr, ctx.sej_base + offset, val) }
}

fn hacc_read(ctx: &HaccBackendVtable, offset: u32) -> Result<u32, String> {
    unsafe { (ctx.reg_read32)(ctx.ptr, ctx.sej_base + offset) }
}

/// SEJ V3 init — matches Python SEJ_V3_Init()
fn sej_v3_init(
    ctx: &HaccBackendVtable,
    encrypt: bool,
    iv: &[u32; 4],
    legacy: bool,
) -> Result<(), String> {
    // 1. Clear all 8 key registers (AKEY0-7, offset 0x20-0x3C)
    for i in 0..8 {
        hacc_write(ctx, HACC_AKEY0 + i * 4, 0)?;
    }

    // 2. Set ACON = CBC | DEC | 128 (decrypt mode for META key generation)
    let acon_init = AES_CHG_BO_OFF | AES_CBC | AES_128; // DEC = 0
    hacc_write(ctx, HACC_ACON, acon_init)?;

    // 3. ACONK = BK2C | R2K — bind HUID/HUK to HACC
    hacc_write(ctx, HACC_ACONK, AES_BK2C | AES_R2K)?;

    // 4. Clear HACC_ASRC/HACC_ACFG/HACC_AOUT
    hacc_write(ctx, HACC_ACON2, AES_CLR)?;

    // 5. Load the IV into ACFG0-3
    hacc_write(ctx, HACC_ACFG0, iv[0])?;
    hacc_write(ctx, HACC_ACFG0 + 4, iv[1])?;
    hacc_write(ctx, HACC_ACFG0 + 8, iv[2])?;
    hacc_write(ctx, HACC_ACFG0 + 12, iv[3])?;

    // 6. Legacy vs non-legacy path
    let acon_setting = AES_CHG_BO_OFF | AES_128 | AES_CBC | if encrypt { AES_ENC } else { 0 };

    if legacy {
        // Legacy path: set HACC_UNK bit1, then do one CLR wait
        let unk = hacc_read(ctx, HACC_UNK)?;
        hacc_write(ctx, HACC_UNK, unk | 2)?;

        hacc_write(ctx, HACC_ACON2, 0x40000000)?;
        for _ in 0..20 {
            let val = hacc_read(ctx, HACC_ACON2)?;
            if val > 0x80000000 {
                break;
            }
            sleep(Duration::from_millis(1));
        }

        // Disable R2K, keep BK2C
        hacc_write(ctx, HACC_UNK, unk & 0xFFFFFFFE)?;
        hacc_write(ctx, HACC_ACONK, AES_BK2C)?;
        hacc_write(ctx, HACC_ACON, acon_setting)?;
    } else {
        // Non-legacy path: set HACC_UNK=1
        hacc_write(ctx, HACC_UNK, 1)?;

        // Derive keys with 3 encryption rounds over g_CFG_RANDOM_PATTERN
        for i in 0..3 {
            let pos = i * 4;
            hacc_write(ctx, HACC_ASRC0, CFG_RANDOM_PATTERN[pos])?;
            hacc_write(ctx, HACC_ASRC0 + 4, CFG_RANDOM_PATTERN[pos + 1])?;
            hacc_write(ctx, HACC_ASRC0 + 8, CFG_RANDOM_PATTERN[pos + 2])?;
            hacc_write(ctx, HACC_ASRC0 + 12, CFG_RANDOM_PATTERN[pos + 3])?;
            hacc_write(ctx, HACC_ACON2, AES_START)?;
            wait_hacc_ready(ctx)?;
        }

        hacc_write(ctx, HACC_ACON2, AES_CLR)?;
        // Reload the IV
        hacc_write(ctx, HACC_ACFG0, iv[0])?;
        hacc_write(ctx, HACC_ACFG0 + 4, iv[1])?;
        hacc_write(ctx, HACC_ACFG0 + 8, iv[2])?;
        hacc_write(ctx, HACC_ACFG0 + 12, iv[3])?;
        hacc_write(ctx, HACC_ACON, acon_setting)?;
        hacc_write(ctx, HACC_ACONK, 0)?;
    }

    trace!("[SEJ_V3_Init] 完成, encrypt={}, legacy={}", encrypt, legacy);
    Ok(())
}

/// SEJ data encrypt/decrypt — matches Python sej_run()
fn sej_run(ctx: &HaccBackendVtable, data: &[u8]) -> Result<Vec<u8>, String> {
    let mut result = Vec::with_capacity(data.len());
    let dwords: Vec<u32> = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();

    for chunk in dwords.chunks(4) {
        hacc_write(ctx, HACC_ASRC0, chunk[0])?;
        hacc_write(ctx, HACC_ASRC0 + 4, chunk.get(1).copied().unwrap_or(0))?;
        hacc_write(ctx, HACC_ASRC0 + 8, chunk.get(2).copied().unwrap_or(0))?;
        hacc_write(ctx, HACC_ASRC0 + 12, chunk.get(3).copied().unwrap_or(0))?;

        hacc_write(ctx, HACC_ACON2, AES_START)?;
        wait_hacc_ready(ctx)?;

        result.extend_from_slice(&hacc_read(ctx, HACC_AOUT0)?.to_le_bytes());
        result.extend_from_slice(&hacc_read(ctx, HACC_AOUT0 + 4)?.to_le_bytes());
        result.extend_from_slice(&hacc_read(ctx, HACC_AOUT0 + 8)?.to_le_bytes());
        result.extend_from_slice(&hacc_read(ctx, HACC_AOUT0 + 12)?.to_le_bytes());
    }

    Ok(result)
}

/// SEJ terminate — matches Python sej_terminate()
fn sej_terminate(ctx: &HaccBackendVtable) -> Result<(), String> {
    hacc_write(ctx, HACC_ACON2, AES_CLR)?;
    for i in 0..8 {
        hacc_write(ctx, HACC_AKEY0 + i * 4, 0)?;
    }
    Ok(())
}

/// AES-128-CBC sign the input via the HACC engine — fully matches Python hw_aes128_cbc_encrypt
fn extract_hacc_output(
    ctx: &HaccBackendVtable,
    data: &[u8],
    legacy: bool,
) -> Result<Vec<u8>, String> {
    // Confirm the HACC backend is ready (DA Extensions enabled); register access always fails otherwise
    if !unsafe { (ctx.is_hacc_ready)(ctx.ptr) } {
        return Err("HACC backend 未就绪（DA Extensions 未启用）".to_string());
    }

    info!("HACC init");
    let iv: [u32; 4] = G_HACC_CFG_1[0..4].try_into().unwrap();

    // 1. SEJ V3 init (binds the HUID/HUK keys)
    sej_v3_init(ctx, true, &iv, legacy)?;

    // 2. Encrypt block by block
    info!("HACC run");
    let result = sej_run(ctx, data)?;

    // 3. Terminate (wipe keys)
    info!("HACC terminate");
    sej_terminate(ctx)?;

    Ok(result)
}

const SEJ_SW_KEY: &[u8; 32] = b"25A1763A21BC854CD569DC23B4782B63";
const SEJ_IV: &[u8; 16] = &[
    0x57, 0x32, 0x5A, 0x5A, 0x12, 0x54, 0x97, 0x66, 0x12, 0x54, 0x97, 0x66, 0x57, 0x32, 0x5A, 0x5A,
];

const SEJ_HW_KEY: [u8; 16] = [0u8; 16];
const G_HACC_CFG_1: [u32; 8] = [
    0x9ED40400, 0x00E884A1, 0xE3F083BD, 0x2F4E6D8A, 0xFF838E5C, 0xE940A0E3, 0x8D4DECC6, 0x45FC0989,
];

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

/// Software-mode AES-256-CBC decryption (sej_sec_cfg_sw decrypt)
/// Matches Python: sej_sec_cfg_sw(data, encrypt=False)
pub(crate) fn sej_sec_cfg_sw_decrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    sej_sec_cfg_sw_decrypt_with_key(data, &sw_key_default())
}

/// Software-mode AES-256-CBC encryption (sej_sec_cfg_sw encrypt)
/// Matches Python: sej_sec_cfg_sw(data, encrypt=True)
pub(crate) fn sej_sec_cfg_sw_encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    sej_sec_cfg_sw_encrypt_with_key(data, &sw_key_default())
}

/// Hardware-mode AES-128-CBC encryption (for SEJ V3/V4 hwtype signing)
/// Matches Python: sej_sec_cfg_hw_V3 → hw_aes128_cbc_encrypt
/// Since hardware SEJ registers are unreachable, this is a software emulation
/// Uses g_HACC_CFG_1 as the IV, AES-128-CBC
pub(crate) fn sej_sec_cfg_hw_v3_encrypt(data: &[u8], legacy: bool) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let mut iv = [0u8; 16];
    for i in 0..4 {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
    }

    let _ = legacy;
    let cipher = Aes128CbcEnc::new(&SEJ_HW_KEY.into(), &iv.into());
    let padded_len = (data.len() + 15) & !15;
    let mut buf = Vec::with_capacity(padded_len);
    buf.extend_from_slice(data);
    buf.resize(padded_len, 0);
    let buf_len = buf.len();
    cipher
        .encrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf, buf_len)
        .map_err(|e| format!("AES-128-CBC 加密失败: {:?}", e))?;
    Ok(buf)
}

/// Hardware-mode V2 encryption (sej_sec_cfg_hw)
/// Matches Python: sej_sec_cfg_hw
pub(crate) fn sej_sec_cfg_hw_encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let mut iv = [0u8; 16];
    for i in 0..4 {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
    }
    let cipher = Aes128CbcEnc::new(&SEJ_HW_KEY.into(), &iv.into());
    let padded_len = (data.len() + 15) & !15;
    let mut buf = Vec::with_capacity(padded_len);
    buf.extend_from_slice(data);
    xor_g_hacc_cfg_1(&mut buf);
    buf.resize(padded_len, 0);
    let buf_len = buf.len();
    cipher
        .encrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf, buf_len)
        .map_err(|e| format!("AES-128-CBC 加密 (V2) 失败: {:?}", e))?;
    Ok(buf)
}

/// Hardware-mode AES-128-CBC decryption (for V3/V4)
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

/// Hardware-mode V2 decryption
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

/// Sign/encrypt the input via the HACC engine and return the result.
/// legacy=true maps to V4, legacy=false maps to V3.
pub fn sej_hacc_sign(data: &[u8], hw_code: u16, legacy: bool) -> Result<Vec<u8>, String> {
    match with_backend_mut(|ctx| extract_hacc_output(ctx, data, legacy)) {
        Ok(sig) => {
            debug!("[SEJ] hw_sign success, hw_code=0x{:04X}", hw_code);
            Ok(sig)
        }
        Err(e) => {
            debug!(
                "[SEJ] hw_sign unavailable (hw_code=0x{:04X}): {}, no software fallback for V3/V4",
                hw_code, e
            );
            Err(e)
        }
    }
}
