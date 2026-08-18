//! 自研 SHA-256 实现（FIPS 180-4），替代 sha2 crate。
//!
//! 仅实现项目实际用到的 API：`Sha256::{new, update, finalize, digest}`。
//! `digest` 接受 `impl AsRef<[u8]>`（兼容 `&[u8]` 与 `Vec<u8>` 调用点），
//! `finalize` 返回 `[u8; 32]`，可直接 `copy_from_slice` 或按 `&[u8]` 使用。
//!
//! SHA-256 为标准算法，错误会表现为哈希值不匹配而非静默数据损坏；
//! 模块内含回归测试向量（`test_sha256_vectors`），可由 `cargo test` 验证。

/// SHA-256 轮常量（FIPS 180-4 §4.2.2）。
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 初始哈希值（FIPS 180-4 §5.3.3）。
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// SHA-256 流式哈希器。
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buf_len: usize,
    /// 已处理的消息总字节数（用于填充阶段的长度域）。
    total_len: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            state: H0,
            buffer: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);

        // 先填满已有缓冲
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buffer[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buffer;
                compress(&mut self.state, &block);
                self.buf_len = 0;
            }
        }

        // 以 64 字节为块处理对齐部分
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            compress(&mut self.state, &block);
            data = &data[64..];
        }

        // 剩余不足一块，暂存缓冲
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    /// 填充并输出 32 字节摘要（消费 hasher）。
    pub fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        // 1. 追加 0x80
        self.pad_byte(0x80);
        // 2. 补 0 直到长度 mod 64 == 56（为 64 位长度预留空间）
        while self.buf_len != 56 {
            self.pad_byte(0x00);
        }
        // 3. 追加 64 位大端比特长度
        for b in bit_len.to_be_bytes() {
            self.pad_byte(b);
        }

        // 4. 状态转为大端字节序列
        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// 一次性计算 `data` 的 SHA-256 摘要。
    pub fn digest(data: impl AsRef<[u8]>) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(data.as_ref());
        h.finalize()
    }

    /// 追加单个填充字节；若缓冲满则压缩并清空。不计入 `total_len`。
    fn pad_byte(&mut self, byte: u8) {
        self.buffer[self.buf_len] = byte;
        self.buf_len += 1;
        if self.buf_len == 64 {
            let block = self.buffer;
            compress(&mut self.state, &block);
            self.buf_len = 0;
        }
    }
}

/// 对单个 64 字节块执行压缩函数（FIPS 180-4 §6.2.2）。
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[4 * i],
            block[4 * i + 1],
            block[4 * i + 2],
            block[4 * i + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    let mut f = state[5];
    let mut g = state[6];
    let mut h = state[7];

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

#[cfg(test)]
mod tests {
    use super::Sha256;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn test_sha256_vectors() {
        // FIPS 180-4 附录 C 测试向量
        assert_eq!(
            hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&Sha256::digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn test_sha256_streaming_matches_one_shot() {
        // 流式 update 应与一次性 digest 结果一致（覆盖缓冲跨块边界路径）
        let data = vec![0xABu8; 1000]; // 非 64 对齐长度
        let mut h = Sha256::new();
        h.update(&data[..500]);
        h.update(&data[500..]);
        let streamed = h.finalize();
        assert_eq!(streamed, Sha256::digest(&data[..]));
        // 与标准库参考比对（hashbrown 不可用，这里仅校验长度与确定性）
        assert_eq!(streamed.len(), 32);
    }
}
