MTKClient-RS



A native Rust tool for low-level flashing and partition management on MediaTek (MTK) chips.

Pure native build, single executable, zero runtime dependencies.

Sky \& cfk99

Overview



MTKClient-RS is a Rust-based low-level flashing tool for MediaTek chips. It supports reading, writing, and erasing device partitions, Preloader/Bootloader unlocking, dynamic partition browsing, and Preloader extraction.



The program is compiled natively into a single .exe and runs without Python / .NET / Visual C++ runtime / libusb DLL.



It communicates with the device's BROM or Preloader mode over USB (WinUSB), and supports DA (Download Agent) session reuse and resume-from-breakpoint, significantly reducing the time required for large partition reads and writes.

Features



&#x20;   Partition read / write / erase: r / w / e for any partition.



&#x20;   GPT management: printgpt prints the full GPT table and generates scatter.txt; r gpt reads raw GPT data.



&#x20;   Bootloader unlock: zyb seccfg unlock/lock unlocks/locks the bootloader via seccfg partition HACC hardware encryption (supports V3/V4); zyb oem unlock/lock, frp for FRP/OEM state.



&#x20;   AVB / vbmeta: zyb vbmeta patches vbmeta to disable/enable AVB verification.



&#x20;   Reboot routing: reboot supports system / fastboot / recovery / fastbootd / meta, with --via (para/misc/da/xml/preloader).



&#x20;   Interactive filesystem: fs\_shell browses the ext4 filesystem inside super directly on the device (ls / cd / cat / cp / tree / info) without pulling images locally first.



&#x20;   Preloader extraction: dumppreloader sends a payload via BROM Exploit to extract Preloader firmware.



&#x20;   Memory read/write: peek / poke directly read/write eMMC memory.



&#x20;   Session reuse \& resume: DA session is reused across commands (.state); large partition reads/writes interrupted by Ctrl+C can resume from the breakpoint.



&#x20;   Automatic driver installation: On first run, WinUSB driver is generated/signed/installed via wdi-rs, no Zadig required.



&#x20;   Dynamic chip loading: Chip and payload mapping is resolved at runtime via data/sdata.json, no hardcoded models.



Supported Chips

Chip	Platform	Status

MT6768	Helio G85 / G88	✅ Supported

MT6769	Helio G85 / G88 series	✅ Supported

MT6771	Helio P60 / P70	✅ Supported

MT8183 / MT8385 / MT8666	Helio P60/P70/G80 series	✅ Supported



In addition, the dynamic chip table in sdata.json supports 6 MediaTek chips. See the support\_chip field in data/sdata.json for details.

Download \& Installation



&#x20;   Download the latest release archive from Releases and extract it to any directory.



&#x20;   Power off the device, hold Volume Up + Volume Down and plug in USB to enter BROM mode (or do nothing to enter Preloader mode).



&#x20;   Right-click and open a terminal as Administrator (PowerShell recommended).



&#x20;   On first run, the WinUSB driver is installed automatically; if that fails, use drivers/installer\_x64.exe to install manually.



Quick Start

powershell



\# Print partition table (also generates scatter.txt)

.\\mtkclient-rs.exe printgpt



\# Read boot\_a partition to a local image

.\\mtkclient-rs.exe r boot\_a boot\_a.img



\# Write boot\_a partition

.\\mtkclient-rs.exe w boot\_a boot\_a.img



\# Erase lk\_b partition

.\\mtkclient-rs.exe e lk\_b



\# Reboot to fastboot

.\\mtkclient-rs.exe reboot fastboot



\# Unlock bootloader (seccfg HACC)

.\\mtkclient-rs.exe zyb seccfg unlock



\# Interactive super filesystem browser

.\\mtkclient-rs.exe fs\_shell



Command Reference

Command	Description

r <partition> \[output]	Read partition to local image

w <partition> <input>	Write local image to partition

e <partition>	Erase partition

printgpt	Print GPT table, generate scatter.txt

r gpt	Read raw GPT data (MBR + GPT header + entries)

reboot \[mode] --via <method>	Reboot device (system/fastboot/recovery/fastbootd/meta)

zyb seccfg unlock/lock	Unlock/lock bootloader via HACC

zyb oem unlock/lock / frp	FRP/OEM unlock and re-lock

zyb vbmeta	Patch vbmeta to disable/enable AVB

zyb erase\_data / wipe\_data	Erase userdata etc., equivalent to factory reset

zyb get\_build\_prop	Read build.prop directly from device partition

slot show/a/b	Show/switch A/B slot

multi	Execute multiple commands in one DA session

adb	Enable ADB debugging in DA mode

fs\_shell	Interactive ext4 filesystem browser inside super

dumppreloader	Extract Preloader via BROM Exploit

peek / poke	Read/write eMMC memory

r boot1/boot2/rpmb	Read special partitions

r/w system/vendor/product/system\_ext	Read/write dynamic partitions inside super (auto slot matching)



Common options: --mode brom|preloader|auto to specify connection mode; --da\_x\_speed <n> to set DA speed level; --data-dir <path> to specify data directory.

Data Directory Structure



The program relies on runtime resources under data/ (payload, DA, chip mapping table), which must be placed alongside the executable:

text



mtkclient-rs.exe

data/

├── generic/

│   └── payload\_xxx.bin        # Generic payload

├── {chip}/                    # Per-chip payload, e.g. mt6768/mt6768\_payload.bin

│   └── mtxxx.bin

├── MTK\_DA\_V5.bin              # DA download agent (core component, do not delete)

└── sdata.json                 # Dynamic chip mapping table (support\_chip / payloads)



&#x20;   data/ is runtime configuration and is not included in the source repository. Place it manually when deploying or when switching --release builds.



Building from Source

bash



\# Requires Rust toolchain (stable)

git clone https://gitee.com/WUNELEZI1/mtkclient-rs.git

cd mtkclient-rs

cargo build --release



\# Output: target/release/mtkclient-rs.exe

\# Place the data/ directory (generic/, per-chip payloads, MTK\_DA\_V5.bin, sdata.json) next to the exe



Safety \& Legal Notice



&#x20;   This tool is intended only for device owners for learning, research, repair, and backup purposes.



&#x20;   Write, erase, and unlock operations are irreversible. Back up important data before operating and ensure image files are correct.



&#x20;   Do not disconnect USB during operation.



&#x20;   Comply with the laws and regulations of your country/region. Do not use this tool on any unauthorized device.



License



Apache-2.0

