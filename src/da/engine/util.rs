//! Pure utility functions used by the engine.

/// Build a trace-level hex dump (16 bytes per line with offset and printable chars).
pub fn hexdump(data: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        out.push_str(&format!("  {:08X}  ", i * 16));
        for j in 0..16 {
            match chunk.get(j) {
                Some(b) => out.push_str(&format!("{:02X} ", b)),
                None => out.push_str("   "),
            }
        }
        out.push(' ');
        out.push('|');
        for b in chunk {
            out.push(if (0x20..0x7F).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        out.push_str("|\n");
    }
    out
}
