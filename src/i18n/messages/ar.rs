//! Arabic (ar-SA) message table and help text.

pub(super) const MESSAGES: &[&str] = &[
    // os_unsupported
    "نظام غير مدعوم: Windows {0}.{1} (البنية {2}). يتطلب Windows 10 أو 11.",
    // arch_unsupported
    "خطأ: هذا البناء يدعم 64-بت (x86_64) فقط.",
    // os_warning
    "تحذير: تم اختباره بالكامل على Windows 10/11 فقط.",
    // get_updates
    "التحديثات:",
    // free_tool_notice
    "هذه الأداة مجانية تمامًا. يُمنع بيعها أو كسرها أو إزالة معلومات المؤلف.",
    // data_dir_missing
    "المسار --data-dir غير موجود أو ليس دليلًا: {0}",
    // elevate_request
    "[MAIN] مطلوب صلاحيات المسؤول لتثبيت تعريف WinUSB؛ جارٍ طلب الرفع...",
    // elevate_failed
    "[MAIN] فشل الرفع: {0}",
    // elevate_hint
    "[MAIN] شغّل كمسؤول (وإلا فلن يكون التبديل إلى WinUSB متاحًا)",
    // connected_preloader
    "متصل (وضع Preloader)",
    // connected_brom
    "متصل (وضع BROM)",
    // no_preloader_forced_dump
    "لم يتم تحديد --preloader؛ سيتم الاستخراج القسري من الجهاز مع استبدال الملف",
    // adb_rebooting
    "تم تفعيل ADB؛ الجهاز يعيد التشغيل إلى النظام",
    // unknown_command
    "أمر غير معروف: {0}",
    // usage_r_gpt
    "الاستخدام: mtkclient r gpt <dir>",
    // usage_rl
    "الاستخدام: mtkclient rl <dir>",
    // usage_wl
    "الاستخدام: mtkclient wl <dir>",
    // usage_w_gpt
    "الاستخدام: mtkclient w gpt <ملف>",
    // read_gpt_failed
    "فشل قراءة GPT: {0}",
    // gpt_unavailable
    "بيانات GPT غير متاحة",
    // parse_gpt_failed
    "فشل تحليل GPT: {0}",
    // no_super_in_gpt
    "لم يتم العثور على قسم super في GPT",
    // log_wait_device
    "في انتظار الجهاز (BROM: Vol+ + Vol- + Power)",
    // log_auto_detect
    "[AUTO] الوضع التلقائي: في انتظار جهاز BROM أو Preloader...",
    // log_brom_usb_found
    "[AUTO] تم اكتشاف جهاز BROM WinUSB",
    // log_brom_com_ok
    "[AUTO] نجحت مصافحة منفذ BROM التسلسلي",
    // log_da_loaded
    "تم تحميل DA",
    // log_da_ext_ready
    "تم تحميل DA Extensions، نوع التخزين: {0}",
    // log_skip_existing
    "  الملف مكتمل بالفعل ({0} بايت)، تم تخطي القراءة",
    // log_saved_to
    "  تم الحفظ في: {0}",
    // log_write_start
    "جارٍ كتابة {0} إلى القسم {1}...",
    // log_write_done
    "  اكتملت الكتابة: {0} بايت إلى القسم {1}",
    // log_verify_ok
    "  تم التحقق بنجاح ({0} بايت)",
    // log_erase_start
    "جارٍ مسح {0}...",
    // log_erased
    "تم مسح {0}",
    // log_format_start
    "جارٍ تهيئة {0} (f2fs)...",
    // log_format_done
    "تمت تهيئة {0} (f2fs)",
    // log_unlock_start
    "جارٍ فتح قفل bootloader عبر الإنترنت...",
    // log_unlock_ok
    "تم فتح قفل bootloader",
    // log_lock_start
    "جارٍ قفل bootloader عبر الإنترنت...",
    // log_lock_ok
    "تم قفل bootloader",
    // log_rpmb_key_ok
    "تم كتابة مفتاح RPMB والتحقق منه",
    // plan_usage
    "الاستخدام: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da ملف]",
    // plan_unknown_op
    "عملية غير معروفة: {0} (المتاح: printgpt/read/write/erase/seccfg/rpmb)",
    // plan_bad_mode
    "--mode غير صالح: {0} (brom/preloader/auto)",
];

pub(super) const HELP: &str = r#"mtkclient-rs
أداة تفليش MTK منخفضة المستوى — تدعم 87 شريحة | Windows 10/11 x64

الاستخدام: mtkclient-rs [خيارات] <أمر> [وسائط]

الأوامر:
  عمليات الأقسام:
    printgpt                    طباعة جدول أقسام GPT + معلومات eMMC + شجرة الأقسام الديناميكية
                                لقسم super
    r <part> <file>             قراءة قسم إلى ملف (كشف تلقائي للأقسام الديناميكية)
      r boot boot.img           قراءة قسم boot
      r boot1 boot1.bin         قراءة eMMC boot1 (قسم خاص)
      r system system.img       قراءة قسم ديناميكي من super تلقائيًا
      r super --dp system_b system_b.img  قراءة قسم ديناميكي محدد داخل super
      r gpt <dir>               تفريغ بيانات GPT الخام إلى دليل
      w gpt <file>              كتابة بيانات GPT الخام (عكس r gpt، لـ eMMC)
    rl <dir> [--skip <...>]     قراءة كل الأقسام إلى دليل (قائمة تخطي مفصولة بفواصل)
    w <part> <file>             كتابة ملف إلى قسم
    wl <dir>                    استعادة كل الأقسام من دليل (.bin/.img)
    e <part>                    مسح قسم
  معلومات:
    zyb get_build_prop [part]   قراءة build.prop (الافتراضي: قسم system)
    slot show|a|b               عرض / تبديل خانة A/B
  الأمان وفتح القفل:
    zyb seccfg unlock           فتح قفل bootloader (seccfg V3/V4 + تعطيل vbmeta تلقائيًا)
    zyb seccfg lock             قفل bootloader (seccfg V3/V4)
    zyb seccfg info [ملف]       عرض حالة seccfg (نوع التشفير، القفل، الصور)
    zyb oem unlock              فتح FRP OEM (تعديل علم قسم frp)
    zyb oem lock                إعادة قفل FRP OEM
    zyb vbmeta <0-3>            تعديل vbmeta (0=إيقاف التحقق 1=تشغيل 2/3=احتياطي)
    unlock [--yes]              فتح Bootloader بخطوة واحدة: seccfg + vbmeta
    fs_shell boot ls|cat|info   تصفح ramdisk الخاص بـ boot (قراءة فقط)
    zyb erase_data              مسح بيانات المستخدم + metadata (إعادة ضبط المصنع)
    frp                         فتح FRP OEM (مرادف zyb oem unlock)
  التحكم بإعادة التشغيل:
    reboot [mode] [--via way]   إعادة تشغيل الجهاز (الافتراضي: system)
      reboot fastboot           إعادة التشغيل إلى bootloader (lk fastboot، وضع BROM فقط)
      reboot recovery           إعادة التشغيل إلى recovery
      reboot fastbootd          إعادة التشغيل إلى fastbootd (fastboot مساحة المستخدم)
      reboot meta               إعادة التشغيل إلى وضع META
      --via misc|para|da|xml|preloader   مسار إعادة التشغيل (الافتراضي: misc)
  متفرقات:
    chain <goal>                عرض سلسلة الاستغلال المخططة لهدف معين
    plan <op>                   عرض المسارات المخططة (>=2) والأسباب، دون اتصال
                                (read|rp|write|peek|dump) — حسب حالة الجهاز
    --sim <preset>              تشغيل دون اتصال على جهاز MTK وهمي
                                (v5-clean|v5-sla|v5-daa|v6|ufs-ab|flaky|unplug)،
                                أو --sim ./my.sim / --port mock://<preset>
    avb <info|disable> <file>   أداة avbtool دون اتصال: تحليل vbmeta / تعطيل التحقق
    fs_shell boot [file]        تصفح تفاعلي لـ ramdisk صورة boot (cpio)
    fs_shell proinfo [file]     فحص تفاعلي لقسم proinfo الخاص بـ MTK (بيانات NV)
    dumppreloader               استخراج preloader من RAM (ثغرة BROM)
    dumpbrom [ملف]              استخراج BROM نفسه (128KB @ 0x0)
    genkey                      توليد مفتاح RPMB عشوائي بحجم 32 بايت (hex)
    rpmb info|init|read|write        برمجة مفتاح RPMB والوصول إلى القسم
    adb                         تفعيل ADB في وضع DA وإعادة التشغيل
    peek <addr> [size]          قراءة ذاكرة الجهاز (4 بايت افتراضيًا)، إخراج hex dump
    poke <addr> <hex>           كتابة بيانات إلى ذاكرة الجهاز
    fs_shell                    تصفح تفاعلي لنظام ملفات قسم super
    multi "<cmd1>;<cmd2>..."    تنفيذ عدة أوامر في جلسة DA واحدة
    run <script.json>           تنفيذ أوامر من ملف سكربت JSON
  help                          عرض هذه المساعدة

الخيارات:
      --da <file>          مسار ملف DA loader
      --da2 <file>         مسار ملف DA2
      --preloader <file>   مسار ملف preloader
      --parttype <type>    نوع القسم
      --offset <n>         عنوان الإزاحة
      --length <n>         الطول
      --sector <n>         رقم القطاع
      --sectors <n>        عدد القطاعات
      --verify             التحقق بعد الكتابة
      --log <1-3>          مستوى السجل: 1=INFO (افتراضي)، 2=DEBUG، 3=TRACE (hex dump خام)
      --quiet              وضع صامت (الأخطاء والنتائج النهائية فقط)
      --quiet_dump         وضع تفريغ صامت (بدون شريط تقدم أو سجلات قراءة USB)
      --usb_log            تتبع اتصالات USB (إلى usb_debug.log)
      --log_file <ملف>     نسخ كل السجلات إلى ملف (وضع الإلحاق)
      --patch_da           تعديل DA (مفعّل افتراضيًا)
      --no_elevate         تخطي فحص رفع صلاحيات المسؤول
      --mode <mode>        وضع العمل: brom (افتراضي) | preloader | auto (محجوز)
      --da_x_speed <1-3>   سرعة تحميل DA: 1=بروتوكول كامل، 2=سريع، 3=أقصى (افتراضي)
      --skip <parts>       أقسام يتم تخطيها في rl (مفصولة بفواصل)
      --data-dir <dir>     دليل البيانات (الافتراضي: data/ بجانب البرنامج)
      --language <LANG>    لغة الإخراج: en-US (افتراضي)، zh-CN، fr-FR، ru-RU، es-ES، ar-SA
  -h, --help               عرض المساعدة
  -V, --version            عرض الإصدار
"#;
