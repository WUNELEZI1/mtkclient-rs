# RPMB sector 与密钥派生

## 结论 1：sector 没有芯片表，必须探测或显式指定

mtkclient `Library/DA/xflash/extension/xflash.py` 的 `custom_rpmb_read(self, sector, sectors)` / `custom_rpmb_write(...)` **接收调用方传入的 sector**，库内没有 `MT6833 → 65504` 之类的表。

→ 落地方式：
1. 默认探测：读 GPT，找 `rpmb` 分区起始 LBA；换算 `sector = lba * 512 / 256`（RPMB 帧 256 字节）。取不到则读 `lk`/`boot` 分区搜索 `RPMB` 字符串。
2. 探测失败 → 不猜，报错并提示 `--rpmb-sector <N>`。
3. `docs` 不维护“机型 → sector”表（无权威来源，且同 SoC 不同 LK 会变）。

## 结论 2：密钥派生用 SEJ，不是 HACC/SSR

mtkclient `custom_rpmb_init`：

```python
def custom_rpmb_init(self, rpmbkey: bytes = None):
    if rpmbkey is None:
        meid = self.get_meid()
        rpmbkey = hwc.aes_hwcrypt(mode="rpmb", data=meid, btype="sej", otp=otp)
    if rpmbkey is not None:
        self.xsend(rpmbkey)
        read_key = self.xread()
        if rpmbkey == read_key:
            self.info("Setting rpmbkey: ok")
```

- 引擎是 **SEJ** 的 `aes_hwcrypt(mode="rpmb")`，输入 MEID，输出 32 字节。
- 之前规划里“需要 HACC/SSR”的判断不准确：HACC 是另一套硬件加密引擎，mtkclient 的 RPMB 路径走 SEJ。
- 我们已有 `security/sej.rs` 与 MEID 读取能力 → **可以自研派生**，不必依赖 HACC。

## 对 mtkclient-rs 的落地要求

- `rpmb set-key`：
  - `--key-file` 提供 32 字节 → 直接用；
  - 否则尝试 `sej::aes_hwcrypt(mode=rpmb, meid)`；SEJ 不可用 → 报“SEJ 不可用，无法派生；请用 --key-file”，**不退回 HACC**；
  - 写完回读校验（`xread` 回显），不一致报错。
- `rpmb read/write` 的 sector：`--rpmb-sector` 优先，其次 GPT 探测，最后报错。
- 现有 `rpmb info|init|set-key|read|write` 命令保留，`init` 走上述派生顺序。

## 证据

- `mtkclient/Library/DA/xflash/extension/xflash.py` 第 478-540 行。
- 错误码表 `rpmb_error`（同文件 36-52 行），与我们 `protocol/rpmb.rs` 的 JEDEC 映射一致。
