# Kamakiri / Kamakiri2 地址与实现（实测源码核对）

## 结论（与规划假设的差异）

1. **mtkclient 的 Kamakiri2 不是“pattern patch 地址表”实现**，而是 **linecode + 控制传输**：
   - 从 BROM 读 linecode：`ctrl_transfer(0xA1, 0x25 or 0x21, 0, 0, 7|8)`；
   - 写回带地址的 linecode：`ctrl_transfer(0x21, 0x20, 0, 0, linecode + pack("<I", addr))`；
   - 触发：`ctrl_transfer(0x80, 0x6, 0x02FF, 0, 9)`。
2. **基准地址来自芯片配置，不是硬编码表**：`ptr_da = chipconfig.brom_register_access[0][1]`；`da_read/da_write` 在 `ptr_da` 附近做 ±3~6 字节的 linecode 微调（`ptr_da + 8 - 3 + i`、`ptr_da - 6 + (4 - i)`、`ptr_da - 5 + (3 - i)`）。
3. **Tinyhack 博客的 pattern 表是另一条路径**（函数 prolog `2d e9 f8 4f 80 46 8a 46`，覆写 `sla_passed` / `skip_auth_1/2` / `send_usb_response` / `usbdl_put_data`），**不是 mtkclient 当前实现**。对没有 `brom_register_access` 配置的芯片才有意义。
4. **Kamakiri（一代）在 mtkclient 中仍保留**，用于 mt6260 及更老；Linux 需禁用 USB 检查、Windows 基本不可用。
5. **KamakiriPl 确实存在**（`Library/Exploit/kamakiripl.py`），是 **Preloader 层**的 linecode 绕过：`initbrom()` 连续 `0xA1/0x21` 取 7 字节 → `0x21/0x20` 写 `0x800000001C200` → 两次 `0x21/0x22 wValue=3` → 再取 linecode → `0x21/0x20` 回写 → `0x21/0x23`。`bypass_2nd_da()` 写 `0x800000001C200 + 0x22C3B0`。**有可运行实现，但无公开漏洞编号/适用芯片清单**。

## 证据

`mtkclient/Library/Exploit/kamakiri2.py`：

```python
def kamakiri2(self, addr):
    self.linecode = self.udev.ctrl_transfer(0xA1, 0x25, 0, 0, 8) + array('B', [0])
    self.udev.ctrl_transfer(0x21, 0x20, 0, 0, self.linecode + array.array('B', pack("<I", addr)))
    self.udev.ctrl_transfer(0x80, 0x6, 0x02FF, 0, 9)

def da_read_write(self, address, length, data=None, ...):
    ptr_da = self.mtk.config.chipconfig.brom_register_access[0][1]
    for i in range(3):
        self.kamakiri2(ptr_da + 8 - 3 + i)
    if address < 0x40:
        for i in range(4):
            self.kamakiri2(ptr_da - 6 + (4 - i))
        return self.mtk.preloader.brom_register_access(address, length, data, check_result)
    else:
        for i in range(3):
            self.kamakiri2(ptr_da - 5 + (3 - i))
        return self.mtk.preloader.brom_register_access(address - 0x40, length, data, check_result)
```

`mtkclient/Library/Exploit/kamakiripl.py`：见上文 `initbrom()` / `bypass_2nd_da()` 的控制序列。

## 对 mtkclient-rs 的落地要求

- 注册表里 Kamakiri2 的 `chips` 不能写“MT67xx/MT68xx 全系”，应按 `brom_register_access` 是否存在于 `CHIP_CONFIGS` 判定；缺配置 → `probe` 返回 `Unknown` 并提示“chip config 缺失，可尝试 dumpbrom 上报”。
- `produces` = `BromRead | BromWrite | BromControlXfer`；若 linecode 阶段成功但 `brom_register_access` 失败，只产出 `BromControlXfer`，不能声称读写。
- Kamakiri（一代）标 `status = DeadCode`（Windows）但保留注册项，便于老芯片用户看到明确原因。
- KamakiriPl 标 `InProgress`：有可运行控制序列，但无芯片清单与成功率数据，**不参与自动 Planner**，仅 `--exploit kamakiripl` 可用。
- 文档与代码都不硬编码 Tinyhack 的 5 个地址；pattern 表只作为“配置缺失时的 fallback 调研项”，未验证前不进代码。

## 待验证

- Tinyhack pattern 表与 mtkclient linecode 两种方法在 MT6789/MT6895 上的实际成功率，无设备，保留。
