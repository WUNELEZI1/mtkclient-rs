# Preloader CVE 调研状态

调研规则：每个 CVE 必须找到**可运行参考代码**才算 `Adapted`；只有分析/复现步骤算 `InProgress`；无公开利用思路算 `DeadCode`。`DeadCode` 不进 Planner。

| CVE | 公开材料（实测） | 结论 |
|---|---|---|
| CVE-2023-20696 | `kasnria001/pwnage24mtk`（Python，41★，7 文件）：伪造 CERT2 ASN.1，把 image header/hash 放到真实证书之前，绕过 preloader 签名校验（bypass_mode 1 遍历任意对象）。另有 `mouseos/mediatek-mt8365-preloader-exploit`（已归档） | **InProgress**：有可运行 Python 代码，但性质是**离线镜像签名伪造**（产出 patched bootloader 再刷写），不是在线 BROM/DA 绕过；Rust 移植工作量大，且与 DA 加载链路无直接关系。不进自动 Planner |
| CVE-2025-20730 | `pwnage24mtk` README 声称同样适用（bypass_mode 0，跳过非 0x30 对象）；CVE 记录为 CWE-287 不安全默认值，无独立 writeup / POC | **DeadCode**：无独立可利用代码，“声称适用”不足以进 Planner |
| CVE-2023-20694 | VulDB 标注无 POC；`mouseos` 有 MT8365 的 `sec_region.c` 退化分析（博客），无可运行代码 | **DeadCode**：无公开 POC |
| CVE-2024-20056 | 仅 `mouseos` 分析提到 `g_dakey` 可被覆盖，未给出完整利用链 | **DeadCode**：无公开 POC |
| pwnage24mtk 通用性 | README 自述 “vibe coded”、“架构检测有问题，需要修” | **不作为可信基础**：仅作参考实现，不移植 |

## 结论对规划的影响

- `PreloaderExploitRegistry` 初始只有 1 条 `InProgress`（CVE-2023-20696 的离线镜像路径），**没有 Adapted 项**。
- 因此 `--mode preloader` 的主路径就是**正常 DA 加载**：`DaLoad(XML/XFlash)`，不需要 exploit。
- Preloader exploit 只在“DA 签名验证失败”时作为**人工选择**的兜底，且当前全部不可用 → 报“无可用 Preloader exploit（见 docs/preloader_cve_status.md）”。
- 不把 `pwnage24mtk` 的“V5/V6 通用”写进任何条件判断。

## 来源

- exploit-intel CVE-2023-20696 页（列出 3 个仓库 POC）
- `kitploit` 对 `pwnage24mtk` 的中文简介
- MediaTek 2023-05 / 2025-11 安全公告
- 上述仓库 README（未经运行验证）
