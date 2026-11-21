//! English (en-US) message table and help text.

pub(super) const MESSAGES: &[&str] = &[
    // os_unsupported
    "Unsupported OS: Windows {0}.{1} (Build {2}). This program requires Windows 10 or Windows 11.",
    // arch_unsupported
    "Error: this build supports only 64-bit (x86_64).",
    // os_warning
    "Warning: fully tested only on Windows 10/11.",
    // get_updates
    "Get updates:",
    // free_tool_notice
    "This tool is completely free. Do not pay for it. Reselling, cracking or removing author info is forbidden.",
    // data_dir_missing
    "--data-dir path does not exist or is not a directory: {0}",
    // elevate_request
    "[MAIN] Administrator rights are required to install the WinUSB driver; requesting elevation...",
    // elevate_failed
    "[MAIN] Elevation failed: {0}",
    // elevate_hint
    "[MAIN] Please right-click and run as administrator (WinUSB switch will be unavailable otherwise)",
    // connected_preloader
    "Connected (Preloader mode)",
    // connected_brom
    "Connected (BROM mode)",
    // no_preloader_forced_dump
    "No --preloader specified; will force-dump from device and overwrite the same-named file",
    // adb_rebooting
    "ADB enabled; device is rebooting into the system",
    // unknown_command
    "Unknown command: {0}",
    // usage_r_gpt
    "usage: mtkclient r gpt <dir>",
    // usage_rl
    "usage: mtkclient rl <dir>",
    // usage_wl
    "usage: mtkclient wl <dir>",
    // usage_w_gpt
    "usage: mtkclient w gpt <file>",
    // read_gpt_failed
    "failed to read GPT: {0}",
    // gpt_unavailable
    "GPT data unavailable",
    // parse_gpt_failed
    "failed to parse GPT: {0}",
    // no_super_in_gpt
    "no super partition found in GPT",
    // log_wait_device
    "Waiting for device (BROM: Vol+ + Vol- + Power)",
    // log_auto_detect
    "[AUTO] Auto mode: waiting for a BROM or Preloader device...",
    // log_brom_usb_found
    "[AUTO] BROM WinUSB device detected",
    // log_brom_com_ok
    "[AUTO] BROM serial handshake succeeded",
    // log_da_loaded
    "DA loaded",
    // log_da_ext_ready
    "DA Extensions loaded, storage type set to {0}",
    // log_skip_existing
    "  File already complete ({0} bytes), skipping read",
    // log_saved_to
    "  Saved to: {0}",
    // log_write_start
    "Writing {0} to partition {1}...",
    // log_write_done
    "  Write complete: {0} bytes written to partition {1}",
    // log_verify_ok
    "  Verification passed ({0} bytes)",
    // log_erase_start
    "Erasing {0}...",
    // log_erased
    "{0} erased",
    // log_format_start
    "Formatting {0} (f2fs)...",
    // log_format_done
    "{0} formatted (f2fs)",
    // log_unlock_start
    "Unlocking the bootloader online...",
    // log_unlock_ok
    "Bootloader unlocked",
    // log_lock_start
    "Locking the bootloader online...",
    // log_lock_ok
    "Bootloader locked",
    // log_rpmb_key_ok
    "RPMB key written and verified by echo",
    // plan_usage
    "usage: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da FILE]",
    // plan_unknown_op
    "unknown op: {0} (available: printgpt/read/write/erase/seccfg/rpmb)",
    // plan_bad_mode
    "invalid --mode: {0} (brom/preloader/auto)",
];

pub(super) const HELP: &str = r#"mtkclient-rs
MTK low-level flashing tool — 87 chipsets supported | Windows 10/11 x64

Usage: mtkclient-rs [OPTIONS] <COMMAND> [ARGS]

Commands:
  Partition operations:
    printgpt                    Print GPT partition table + eMMC info + super dynamic
                                partition tree
    r <part> <file>             Read a partition to a file (dynamic partitions detected)
      r boot boot.img           Read the boot partition
      r boot1 boot1.bin         Read eMMC boot1 (special partition)
      r system system.img       Read a dynamic partition from super automatically
      r super --dp system_b system_b.img  Read a specific dynamic partition in super
      r gpt <dir>               Dump raw GPT data to a directory
      w gpt <file>              Write raw GPT data back (inverse of r gpt, eMMC)
    rl <dir> [--skip <...>]     Read all partitions to a directory (comma-separated skip list)
    w <part> <file>             Write a file to a partition (dynamic partitions detected)
    wl <dir>                    Restore all partitions from a directory (.bin/.img matched)
    e <part>                    Erase a partition
  Information:
    zyb get_build_prop [part]   Read build.prop (default: system partition)
    slot show|a|b               Show / switch the A/B slot
  Security & unlocking:
    zyb seccfg unlock           Unlock the bootloader (seccfg V3/V4 + auto vbmeta disable)
    zyb seccfg lock             Lock the bootloader (seccfg V3/V4)
    zyb seccfg info [file]      Show seccfg state (hwtype, lock state, image list)
    zyb oem unlock              FRP OEM unlock (patches the frp partition flag)
    zyb oem lock                FRP OEM relock
    zyb vbmeta <0-3>            Patch vbmeta (0=verification off, 1=on, 2/3=reserved)
    unlock [--yes]              One-shot unlock: seccfg unlock + disable vbmeta checks
    fs_shell boot ls|cat|info   Browse boot ramdisk (read-only, capability-gated)
    fs_shell proinfo show       Parse proinfo NV blocks
    zyb erase_data              Wipe user data + metadata (factory reset)
    frp                         FRP OEM unlock (alias of zyb oem unlock)
  Reboot control:
    reboot [mode] [--via way]   Reboot the device (default: system)
      reboot fastboot           Reboot to the bootloader (lk fastboot, BROM mode only)
      reboot recovery           Reboot to recovery
      reboot fastbootd          Reboot to fastbootd (userspace fastboot)
      reboot meta               Reboot to META mode
      --via misc|para|da|xml|preloader   Reboot path (default: misc)
  Misc:
    chain <goal>                Show the planned smart exploit chain for a goal
    plan <op>                   Show planned paths (>=2) and reasons, offline
                                (read|rp|write|peek|dump) — device-state aware
    --sim <preset>              Run fully offline against a simulated MTK device
                                (v5-clean|v5-sla|v5-daa|v6|ufs-ab|flaky|unplug),
                                or --sim ./my.sim / --port mock://<preset>
    avb <info|disable> <file>   Offline avbtool: parse vbmeta / disable verification
    fs_shell boot [file]        Interactively browse a boot image ramdisk (cpio):
                                ls/cat/extract/info/quit; from the active slot if no file
    fs_shell proinfo [file]     Interactively inspect the MTK proinfo partition (NV data)
    dumppreloader               Extract the preloader from RAM (BROM exploit)
    dumpbrom [file]             Extract the BROM itself (128KB @ 0x0) for analysis
    genkey                      Generate a random 32-byte RPMB key (hex)
    rpmb info|init|read|write        RPMB key programming and partition access
    adb                         Enable ADB in DA mode and reboot
    peek <addr> [size]          Read device memory (default 4 bytes), hex dump output
    poke <addr> <hex>           Write data to device memory
    fs_shell                    Interactively browse the super partition filesystem
    multi "<cmd1>;<cmd2>..."    Run multiple commands in one DA session
    run <script.json>           Run commands from a JSON script file
  help                          Show this help

Options:
      --da <file>          DA loader file path
      --da2 <file>         DA2 file path
      --preloader <file>   Preloader file path
      --parttype <type>    Partition type
      --offset <n>         Offset address
      --length <n>         Length
      --sector <n>         Sector number
      --sectors <n>        Sector count
      --verify             Verify after writing
      --log <1-3>          Log level: 1=INFO (default), 2=DEBUG, 3=TRACE (raw hex dump)
      --quiet              Quiet mode (errors and final results only)
      --quiet_dump         Quiet dump mode (no progress bar / USB read logs)
      --usb_log            USB communication trace log (to usb_debug.log)
      --log_file <FILE>    Tee all logs to FILE (append mode)
      --patch_da           Patch DA (default: on)
      --no_elevate         Skip administrator elevation check
      --mode <mode>        Work mode: brom (default) | preloader | auto (reserved)
      --da_x_speed <1-3>   DA load speed: 1=full protocol, 2=fast, 3=fastest (default)
      --skip <parts>       Partitions to skip for rl (comma-separated)
      --data-dir <dir>     Data directory (default: data/ next to the program)
      --language <LANG>    Output language: en-US (default), zh-CN, fr-FR, ru-RU, es-ES, ar-SA
  -h, --help               Show help
  -V, --version            Show version
"#;
