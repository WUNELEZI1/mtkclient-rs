# DA 协议版本（V5 / V6）判定

## 结论

**协议版本以 DA 文件头为准，不以芯片型号表为准。**

mtkclient `Library/DA/daconfig.py`：

```python
hdr = bootldr.read(0x68)
v6 = b"MTK_DA_v6" in hdr
...
if loader.v6:
    self.config.chipconfig.damode = DAmodes.XML
```

即：DA 包头部 0x68 字节内出现 `MTK_DA_v6` → XML（V6）；否则 XFlash（V5）。

## 对规划的影响

- `DeviceState.protocol_version` 应有两个来源：
  1. **DaBinary 解析结果**（权威）：`DaBinary::protocol()` 读头部标识；
  2. **芯片表预判**（仅用于未提供 DA 时给提示）：`ChipConfig.da_code` + 社区表，标 `Confidence::Low`。
- `docs` 中不写“MT6983 起 V6”这类结论：未在 mtkclient / penumbra 源码找到该映射表，属社区说法。
- Planner 排除 BROM 路径的条件必须是“DA 已解析且为 V6”或“芯片在已知 V6 名单且用户提供 DA”，不能仅凭 hw_code 猜。

## 已知 V6 名单（来自 mtkclient README 中文版，未在代码中二次验证）

MT6781 / MT6789 / MT6855 / MT6886 / MT6895 / MT6983 / MT8985：BootROM 漏洞已修复，必须 `--loader` 指定 DA，BROM 路径不可用。

落地方式：`KNOWN_V6_HW_CODES` 常量表 + `docs` 注明来源为 README，probe 只能据此返回 `Low` 置信度。
