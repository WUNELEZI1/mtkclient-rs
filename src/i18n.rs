//! CLI 输出国际化（i18n）。
//!
//! 默认 `en-US`；支持 `en/en-US`、`zh-CN`、`fr/fr-FR`、`ru/ru-RU`、`es/es-ES`、`ar/ar-SA`。
//! 语言来源优先级：命令行 `--language <LANG>`（任意位置，
//! 如 `mtkclient-rs help --language zh-CN`）> `MTK_LANG` > `LC_ALL`/`LANG`。
//! 说明：设备返回的原始文本不翻译；工具自身的流程日志按“高频优先”逐步迁移到本模块
//! （连接 / DA 加载 / 分区读写 / seccfg / RPMB 已迁移），未迁移的深层调试日志保持中文。

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En = 0,
    ZhCn = 1,
    Fr = 2,
    Ru = 3,
    Es = 4,
    Ar = 5,
}

const LANG_COUNT: usize = 6;
static CURRENT: AtomicUsize = AtomicUsize::new(Lang::En as usize);

pub fn lang() -> Lang {
    match CURRENT.load(Ordering::SeqCst) {
        1 => Lang::ZhCn,
        2 => Lang::Fr,
        3 => Lang::Ru,
        4 => Lang::Es,
        5 => Lang::Ar,
        _ => Lang::En,
    }
}

pub fn parse(s: &str) -> Option<Lang> {
    let v = s.trim().to_ascii_lowercase().replace('_', "-");
    match v.as_str() {
        "en" | "en-us" | "en-gb" | "english" => Some(Lang::En),
        "zh" | "zh-cn" | "zh-hans" | "zh-sg" | "chinese" => Some(Lang::ZhCn),
        "fr" | "fr-fr" | "fr-ca" | "french" => Some(Lang::Fr),
        "ru" | "ru-ru" | "russian" => Some(Lang::Ru),
        "es" | "es-es" | "es-mx" | "es-ar" | "spanish" => Some(Lang::Es),
        "ar" | "ar-sa" | "ar-eg" | "arabic" => Some(Lang::Ar),
        _ => None,
    }
}

fn set(l: Lang) {
    CURRENT.store(l as usize, Ordering::SeqCst);
}

/// 提取 `--language/--lang`（任意位置），返回剩余参数。
pub fn extract_from_args(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let inline = a
            .strip_prefix("--language=")
            .or_else(|| a.strip_prefix("--lang="));
        if let Some(v) = inline {
            if let Some(l) = parse(v) {
                set(l);
            } else {
                eprintln!("mtkclient-rs: warning: unsupported language '{v}', using en-US");
            }
            i += 1;
        } else if a == "--language" || a == "--lang" {
            if let Some(v) = args.get(i + 1) {
                if let Some(l) = parse(v) {
                    set(l);
                } else {
                    eprintln!("mtkclient-rs: warning: unsupported language '{v}', using en-US");
                }
                i += 2;
            } else {
                eprintln!("mtkclient-rs: warning: --language requires a value, using en-US");
                i += 1;
            }
        } else {
            out.push(a.clone());
            i += 1;
        }
    }
    out
}

/// 环境回退：`MTK_LANG` > `LC_ALL` > `LANG`。
pub fn detect_env() {
    for var in ["MTK_LANG", "LC_ALL", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            let head = v.split(['.', '@']).next().unwrap_or("");
            if let Some(l) = parse(head) {
                set(l);
                return;
            }
        }
    }
}

static MSGS: &[(&str, [&str; LANG_COUNT])] = &[
    (
        "os_unsupported",
        [
            "Unsupported OS: Windows {0}.{1} (Build {2}). This program requires Windows 10 or Windows 11.",
            "不支持的操作系统: Windows {0}.{1} (Build {2})。本程序需要 Windows 10 或 Windows 11。",
            "OS non pris en charge : Windows {0}.{1} (Build {2}). Windows 10 ou 11 requis.",
            "Неподдерживаемая ОС: Windows {0}.{1} (сборка {2}). Требуется Windows 10 или 11.",
            "SO no compatible: Windows {0}.{1} (compilación {2}). Se requiere Windows 10 u 11.",
            "نظام غير مدعوم: Windows {0}.{1} (البنية {2}). يتطلب Windows 10 أو 11.",
        ],
    ),
    (
        "arch_unsupported",
        [
            "Error: this build supports only 64-bit (x86_64).",
            "错误: 本程序仅支持 64-bit (x86_64) 架构。",
            "Erreur : cette build ne prend en charge que le 64 bits (x86_64).",
            "Ошибка: эта сборка поддерживает только 64-bit (x86_64).",
            "Error: esta compilación solo admite 64 bits (x86_64).",
            "خطأ: هذا البناء يدعم 64-بت (x86_64) فقط.",
        ],
    ),
    (
        "os_warning",
        [
            "Warning: fully tested only on Windows 10/11.",
            "警告: 本程序仅在 Windows 10/11 上经过充分测试。",
            "Avertissement : testé uniquement sur Windows 10/11.",
            "Предупреждение: полностью протестировано только на Windows 10/11.",
            "Advertencia: probado completamente solo en Windows 10/11.",
            "تحذير: تم اختباره بالكامل على Windows 10/11 فقط.",
        ],
    ),
    (
        "get_updates",
        [
            "Get updates:",
            "获取更新:",
            "Mises à jour :",
            "Обновления:",
            "Actualizaciones:",
            "التحديثات:",
        ],
    ),
    (
        "free_tool_notice",
        [
            "This tool is completely free. Do not pay for it. Reselling, cracking or removing author info is forbidden.",
            "本工具完全免费，请勿被骗！禁止任何形式的倒卖、破解或去除作者信息。违者必究。",
            "Cet outil est entièrement gratuit. Revente, crack ou suppression des mentions d'auteur interdits.",
            "Этот инструмент полностью бесплатен. Перепродажа, взлом или удаление информации об авторе запрещены.",
            "Esta herramienta es totalmente gratuita. Prohibida su reventa, crack o eliminación de la autoría.",
            "هذه الأداة مجانية تمامًا. يُمنع بيعها أو كسرها أو إزالة معلومات المؤلف.",
        ],
    ),
    (
        "data_dir_missing",
        [
            "--data-dir path does not exist or is not a directory: {0}",
            "--data-dir 路径不存在或不是目录: {0}",
            "le chemin --data-dir n'existe pas ou n'est pas un répertoire : {0}",
            "путь --data-dir не существует или не является каталогом: {0}",
            "la ruta --data-dir no existe o no es un directorio: {0}",
            "المسار --data-dir غير موجود أو ليس دليلًا: {0}",
        ],
    ),
    (
        "elevate_request",
        [
            "[MAIN] Administrator rights are required to install the WinUSB driver; requesting elevation...",
            "[MAIN] 需要管理员权限以安装 WinUSB 驱动，正在请求提权...",
            "[MAIN] Droits administrateur requis pour installer le pilote WinUSB ; élévation en cours...",
            "[MAIN] Для установки драйвера WinUSB нужны права администратора; запрос повышения...",
            "[MAIN] Se requieren derechos de administrador para instalar el controlador WinUSB; elevando...",
            "[MAIN] مطلوب صلاحيات المسؤول لتثبيت تعريف WinUSB؛ جارٍ طلب الرفع...",
        ],
    ),
    (
        "elevate_failed",
        [
            "[MAIN] Elevation failed: {0}",
            "[MAIN] 提权失败: {0}",
            "[MAIN] Échec de l'élévation : {0}",
            "[MAIN] Ошибка повышения прав: {0}",
            "[MAIN] Error de elevación: {0}",
            "[MAIN] فشل الرفع: {0}",
        ],
    ),
    (
        "elevate_hint",
        [
            "[MAIN] Please right-click and run as administrator (WinUSB switch will be unavailable otherwise)",
            "[MAIN] 请右键以管理员身份运行本程序（否则将无法切换 WinUSB）",
            "[MAIN] Veuillez exécuter en tant qu'administrateur (sinon WinUSB ne pourra pas être activé)",
            "[MAIN] Запустите от имени администратора (иначе переключение WinUSB будет недоступно)",
            "[MAIN] Ejecute como administrador (de lo contrario no se podrá cambiar a WinUSB)",
            "[MAIN] شغّل كمسؤول (وإلا فلن يكون التبديل إلى WinUSB متاحًا)",
        ],
    ),
    (
        "connected_preloader",
        [
            "Connected (Preloader mode)",
            "连接成功 (Preloader 模式)",
            "Connecté (mode Preloader)",
            "Подключено (режим Preloader)",
            "Conectado (modo Preloader)",
            "متصل (وضع Preloader)",
        ],
    ),
    (
        "connected_brom",
        [
            "Connected (BROM mode)",
            "连接成功 (BROM 模式)",
            "Connecté (mode BROM)",
            "Подключено (режим BROM)",
            "Conectado (modo BROM)",
            "متصل (وضع BROM)",
        ],
    ),
    (
        "no_preloader_forced_dump",
        [
            "No --preloader specified; will force-dump from device and overwrite the same-named file",
            "未指定 --preloader，将强制从设备 dump 并覆盖同名文件",
            "Aucun --preloader spécifié ; extraction forcée depuis l'appareil avec écrasement du fichier",
            "--preloader не указан; будет принудительно извлечён с устройства с перезаписью файла",
            "No se especificó --preloader; se extraerá del dispositivo sobrescribiendo el archivo",
            "لم يتم تحديد --preloader؛ سيتم الاستخراج القسري من الجهاز مع استبدال الملف",
        ],
    ),
    (
        "adb_rebooting",
        [
            "ADB enabled; device is rebooting into the system",
            "ADB 已启用，设备正在重启进入系统",
            "ADB activé ; l'appareil redémarre vers le système",
            "ADB включён; устройство перезагружается в систему",
            "ADB activado; el dispositivo reinicia al sistema",
            "تم تفعيل ADB؛ الجهاز يعيد التشغيل إلى النظام",
        ],
    ),
    (
        "unknown_command",
        [
            "Unknown command: {0}",
            "未知命令: {0}",
            "Commande inconnue : {0}",
            "Неизвестная команда: {0}",
            "Comando desconocido: {0}",
            "أمر غير معروف: {0}",
        ],
    ),
    (
        "usage_r_gpt",
        [
            "usage: mtkclient r gpt <dir>",
            "用法: mtkclient r gpt <dir>",
            "usage : mtkclient r gpt <dir>",
            "использование: mtkclient r gpt <dir>",
            "uso: mtkclient r gpt <dir>",
            "الاستخدام: mtkclient r gpt <dir>",
        ],
    ),
    (
        "usage_rl",
        [
            "usage: mtkclient rl <dir>",
            "用法: mtkclient rl <dir>",
            "usage : mtkclient rl <dir>",
            "использование: mtkclient rl <dir>",
            "uso: mtkclient rl <dir>",
            "الاستخدام: mtkclient rl <dir>",
        ],
    ),
    (
        "usage_wl",
        [
            "usage: mtkclient wl <dir>",
            "用法: mtkclient wl <dir>",
            "usage : mtkclient wl <dir>",
            "использование: mtkclient wl <dir>",
            "uso: mtkclient wl <dir>",
            "الاستخدام: mtkclient wl <dir>",
        ],
    ),
    (
        "usage_w_gpt",
        [
            "usage: mtkclient w gpt <file>",
            "用法: mtkclient w gpt <文件>",
            "usage : mtkclient w gpt <fichier>",
            "использование: mtkclient w gpt <файл>",
            "uso: mtkclient w gpt <archivo>",
            "الاستخدام: mtkclient w gpt <ملف>",
        ],
    ),
    (
        "read_gpt_failed",
        [
            "failed to read GPT: {0}",
            "读取 GPT 失败: {0}",
            "échec de la lecture du GPT : {0}",
            "не удалось прочитать GPT: {0}",
            "error al leer el GPT: {0}",
            "فشل قراءة GPT: {0}",
        ],
    ),
    (
        "gpt_unavailable",
        [
            "GPT data unavailable",
            "GPT 数据不可用",
            "données GPT indisponibles",
            "данные GPT недоступны",
            "datos GPT no disponibles",
            "بيانات GPT غير متاحة",
        ],
    ),
    (
        "parse_gpt_failed",
        [
            "failed to parse GPT: {0}",
            "解析 GPT 失败: {0}",
            "échec de l'analyse du GPT : {0}",
            "не удалось разобрать GPT: {0}",
            "error al analizar el GPT: {0}",
            "فشل تحليل GPT: {0}",
        ],
    ),
    (
        "no_super_in_gpt",
        [
            "no super partition found in GPT",
            "GPT 中未找到 super 分区",
            "aucune partition super dans le GPT",
            "раздел super не найден в GPT",
            "no se encontró la partición super en el GPT",
            "لم يتم العثور على قسم super في GPT",
        ],
    ),
    // ── 深层流程日志（连接 / DA / 分区读写 / seccfg）─────────────────
    (
        "log_wait_device",
        [
            "Waiting for device (BROM: Vol+ + Vol- + Power)",
            "等待设备连接 (BROM: Vol+ + Vol- + Power)",
            "En attente de l'appareil (BROM : Vol+ + Vol- + Power)",
            "Ожидание устройства (BROM: Vol+ + Vol- + Power)",
            "Esperando el dispositivo (BROM: Vol+ + Vol- + Power)",
            "في انتظار الجهاز (BROM: Vol+ + Vol- + Power)",
        ],
    ),
    (
        "log_auto_detect",
        [
            "[AUTO] Auto mode: waiting for a BROM or Preloader device...",
            "[AUTO] 自动检测模式：等待 BROM 或 Preloader 设备出现...",
            "[AUTO] Mode auto : attente d'un appareil BROM ou Preloader...",
            "[AUTO] Авторежим: ожидание устройства BROM или Preloader...",
            "[AUTO] Modo automático: esperando un dispositivo BROM o Preloader...",
            "[AUTO] الوضع التلقائي: في انتظار جهاز BROM أو Preloader...",
        ],
    ),
    (
        "log_brom_usb_found",
        [
            "[AUTO] BROM WinUSB device detected",
            "[AUTO] 检测到 BROM WinUSB 设备",
            "[AUTO] Appareil BROM WinUSB détecté",
            "[AUTO] Обнаружено устройство BROM WinUSB",
            "[AUTO] Dispositivo BROM WinUSB detectado",
            "[AUTO] تم اكتشاف جهاز BROM WinUSB",
        ],
    ),
    (
        "log_brom_com_ok",
        [
            "[AUTO] BROM serial handshake succeeded",
            "[AUTO] BROM 串口握手成功",
            "[AUTO] Poignée de main série BROM réussie",
            "[AUTO] Рукопожатие по COM-порту BROM успешно",
            "[AUTO] Handshake serie BROM correcto",
            "[AUTO] نجحت مصافحة منفذ BROM التسلسلي",
        ],
    ),
    (
        "log_da_loaded",
        [
            "DA loaded",
            "DA 加载完成",
            "DA chargé",
            "DA загружен",
            "DA cargado",
            "تم تحميل DA",
        ],
    ),
    (
        "log_da_ext_ready",
        [
            "DA Extensions loaded, storage type set to {0}",
            "DA Extensions 加载成功，存储类型已设置为 {0}",
            "DA Extensions chargé, type de stockage : {0}",
            "DA Extensions загружены, тип хранилища: {0}",
            "DA Extensions cargado, tipo de almacenamiento: {0}",
            "تم تحميل DA Extensions، نوع التخزين: {0}",
        ],
    ),
    (
        "log_skip_existing",
        [
            "  File already complete ({0} bytes), skipping read",
            "  文件已存在且完整 ({0} 字节)，跳过读取",
            "  Fichier déjà complet ({0} octets), lecture ignorée",
            "  Файл уже полон ({0} байт), чтение пропущено",
            "  Archivo ya completo ({0} bytes), se omite la lectura",
            "  الملف مكتمل بالفعل ({0} بايت)، تم تخطي القراءة",
        ],
    ),
    (
        "log_saved_to",
        [
            "  Saved to: {0}",
            "  已保存到: {0}",
            "  Enregistré dans : {0}",
            "  Сохранено в: {0}",
            "  Guardado en: {0}",
            "  تم الحفظ في: {0}",
        ],
    ),
    (
        "log_write_start",
        [
            "Writing {0} to partition {1}...",
            "写入文件 {0} 到分区 {1}...",
            "Écriture de {0} vers la partition {1}...",
            "Запись {0} в раздел {1}...",
            "Escribiendo {0} en la partición {1}...",
            "جارٍ كتابة {0} إلى القسم {1}...",
        ],
    ),
    (
        "log_write_done",
        [
            "  Write complete: {0} bytes written to partition {1}",
            "  写入完成: {0} 字节写入分区 {1}",
            "  Écriture terminée : {0} octets écrits dans {1}",
            "  Запись завершена: {0} байт записано в раздел {1}",
            "  Escritura completada: {0} bytes en la partición {1}",
            "  اكتملت الكتابة: {0} بايت إلى القسم {1}",
        ],
    ),
    (
        "log_verify_ok",
        [
            "  Verification passed ({0} bytes)",
            "  校验通过 (共 {0} 字节)",
            "  Vérification réussie ({0} octets)",
            "  Проверка пройдена ({0} байт)",
            "  Verificación correcta ({0} bytes)",
            "  تم التحقق بنجاح ({0} بايت)",
        ],
    ),
    (
        "log_erase_start",
        [
            "Erasing {0}...",
            "正在擦除 {0}...",
            "Effacement de {0}...",
            "Стирание {0}...",
            "Borrando {0}...",
            "جارٍ مسح {0}...",
        ],
    ),
    (
        "log_erased",
        [
            "{0} erased",
            "{0} 已擦除",
            "{0} effacé",
            "{0} стёрт",
            "{0} borrado",
            "تم مسح {0}",
        ],
    ),
    (
        "log_format_start",
        [
            "Formatting {0} (f2fs)...",
            "正在格式化 {0} (f2fs)...",
            "Formatage de {0} (f2fs)...",
            "Форматирование {0} (f2fs)...",
            "Formateando {0} (f2fs)...",
            "جارٍ تهيئة {0} (f2fs)...",
        ],
    ),
    (
        "log_format_done",
        [
            "{0} formatted (f2fs)",
            "{0} 已格式化 (f2fs)",
            "{0} formaté (f2fs)",
            "{0} отформатирован (f2fs)",
            "{0} formateado (f2fs)",
            "تمت تهيئة {0} (f2fs)",
        ],
    ),
    (
        "log_unlock_start",
        [
            "Unlocking the bootloader online...",
            "开始在线解锁 Bootloader...",
            "Déverrouillage du bootloader en ligne...",
            "Разблокировка bootloader онлайн...",
            "Desbloqueando el bootloader en línea...",
            "جارٍ فتح قفل bootloader عبر الإنترنت...",
        ],
    ),
    (
        "log_unlock_ok",
        [
            "Bootloader unlocked",
            "Bootloader 解锁成功",
            "Bootloader déverrouillé",
            "Bootloader разблокирован",
            "Bootloader desbloqueado",
            "تم فتح قفل bootloader",
        ],
    ),
    (
        "log_lock_start",
        [
            "Locking the bootloader online...",
            "开始在线锁定 Bootloader...",
            "Verrouillage du bootloader en ligne...",
            "Блокировка bootloader онлайн...",
            "Bloqueando el bootloader en línea...",
            "جارٍ قفل bootloader عبر الإنترنت...",
        ],
    ),
    (
        "log_lock_ok",
        [
            "Bootloader locked",
            "Bootloader 锁定成功",
            "Bootloader verrouillé",
            "Bootloader заблокирован",
            "Bootloader bloqueado",
            "تم قفل bootloader",
        ],
    ),
    (
        "log_rpmb_key_ok",
        [
            "RPMB key written and verified by echo",
            "RPMB 密钥已写入并通过回显校验",
            "Clé RPMB écrite et vérifiée par écho",
            "RPMB-ключ записан и подтверждён эхом",
            "Clave RPMB escrita y verificada por eco",
            "تم كتابة مفتاح RPMB والتحقق منه",
        ],
    ),
    (
        "plan_usage",
        [
            "usage: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da FILE]",
            "用法: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da 文件]",
            "usage : mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da FICHIER]",
            "использование: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da ФАЙЛ]",
            "uso: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da ARCHIVO]",
            "الاستخدام: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da ملف]",
        ],
    ),
    (
        "plan_unknown_op",
        [
            "unknown op: {0} (available: printgpt/read/write/erase/seccfg/rpmb)",
            "未知操作: {0}（可用: printgpt/read/write/erase/seccfg/rpmb）",
            "opération inconnue : {0} (disponibles : printgpt/read/write/erase/seccfg/rpmb)",
            "неизвестная операция: {0} (доступно: printgpt/read/write/erase/seccfg/rpmb)",
            "operación desconocida: {0} (disponibles: printgpt/read/write/erase/seccfg/rpmb)",
            "عملية غير معروفة: {0} (المتاح: printgpt/read/write/erase/seccfg/rpmb)",
        ],
    ),
    (
        "plan_bad_mode",
        [
            "invalid --mode: {0} (brom/preloader/auto)",
            "非法 --mode: {0}（brom/preloader/auto）",
            "--mode invalide : {0} (brom/preloader/auto)",
            "неверный --mode: {0} (brom/preloader/auto)",
            "--mode no válido: {0} (brom/preloader/auto)",
            "--mode غير صالح: {0} (brom/preloader/auto)",
        ],
    ),
];

pub fn t(key: &str) -> &'static str {
    let idx = lang() as usize;
    for (k, texts) in MSGS {
        if *k == key {
            return texts[idx];
        }
    }
    ""
}

pub fn tr(key: &str, args: &[&str]) -> String {
    let mut s = t(key).to_string();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), a);
    }
    s
}

/// 当前语言下的完整帮助文本。
pub fn help() -> &'static str {
    match lang() {
        Lang::En => HELP_EN,
        Lang::ZhCn => HELP_ZH_CN,
        Lang::Fr => HELP_FR,
        Lang::Ru => HELP_RU,
        Lang::Es => HELP_ES,
        Lang::Ar => HELP_AR,
    }
}

const HELP_EN: &str = r#"mtkclient-rs
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

const HELP_ZH_CN: &str = r#"mtkclient-rs
MTK 设备底层刷机工具 - 支持 87 种芯片 | Windows 10/11 x64

用法: mtkclient-rs [选项] <命令> [参数]

命令:
  分区操作:
    printgpt                    打印 GPT 分区表 + eMMC 信息 + super 动态分区树
    r <part> <file>             读取分区到文件（自动识别动态分区）
      r boot boot.img           读取 boot 分区
      r boot1 boot1.bin         读取 eMMC boot1（特殊分区）
      r system system.img       自动从 super 读取动态分区
      r super --dp system_b system_b.img  读取 super 内指定动态分区
      r gpt <dir>               读取 GPT 原始数据到目录
      w gpt <file>              写回 GPT 原始数据（r gpt 的逆操作，eMMC）
    rl <dir> [--skip <...>]     读取全部分区到目录（逗号分隔跳过列表）
    w <part> <file>             写入文件到分区（自动识别动态分区）
    wl <dir>                    从目录恢复全部分区（匹配 .bin/.img）
    e <part>                    擦除分区
  信息查询:
    zyb get_build_prop [part]   读取 build.prop（默认 system 分区）
    slot show|a|b               显示/切换 A/B 槽位
  安全与解锁:
    zyb seccfg unlock           解锁 Bootloader（seccfg V3/V4 + 自动禁用 vbmeta）
    zyb seccfg lock             锁定 Bootloader（seccfg V3/V4）
    zyb seccfg info [文件]      查看 seccfg 状态（hwtype/锁状态/镜像列表）
    zyb oem unlock              FRP OEM 解锁（修改 frp 分区标志位）
    zyb oem lock                FRP OEM 回锁
    zyb vbmeta <0-3>            修补 vbmeta（0=关闭验证 1=启用 2/3=保留）
    zyb erase_data              清除用户数据 + metadata（恢复出厂设置）
    frp                         FRP OEM 解锁（zyb oem unlock 的别名）
  重启控制:
    reboot [mode] [--via way]   重启设备（默认 system）
      reboot fastboot           重启到 Bootloader（lk fastboot，仅 BROM 模式）
      reboot recovery           重启到 Recovery
      reboot fastbootd          重启到 fastbootd（userspace fastboot）
      reboot meta               重启到 META 模式
      --via misc|para|da|xml|preloader   重启途径（默认 misc）
  其他:
    chain <goal>                显示目标的智能利用链规划（read|rp|write|peek|dump），
    plan <op>                   离线查看规划路径（≥2 条）与选择理由，不连接设备
                                按设备状态动态选择 kamakiri/carbonara 等最优路径
    avb <info|disable> <file>   离线 avbtool：解析 vbmeta / 禁用校验（自研，无需设备）
    fs_shell boot [file]        交互式浏览 boot 镜像 ramdisk（cpio）：ls/cat/extract/quit；
                                省略文件时自动读当前 A/B 槽位的 boot 分区
    fs_shell proinfo [file]     交互式查看 MTK proinfo 分区（NV 数据）
    dumppreloader               从 RAM 提取 Preloader（BROM Exploit）
    dumpbrom [file]             提取 BROM 本体（0x0 起 128KB，供逆向分析）
    genkey                      生成 32 字节随机 RPMB 密钥（hex 输出）
    rpmb info|init|read|write        RPMB 密钥烧写与分区读写
    adb                         在 DA 模式下开启 ADB 并重启
    peek <addr> [size]          读取设备内存（默认 4 字节），输出 hex dump
    poke <addr> <hex>           写入数据到设备内存
    fs_shell                    交互式浏览 super 分区文件系统
    multi "<cmd1>;<cmd2>..."    一次 DA 会话执行多个命令
    run <script.json>           从 JSON 脚本文件批量执行命令
  help                          显示本帮助

选项:
      --da <file>          指定 DA loader 文件路径
      --da2 <file>         指定 DA2 文件路径
      --preloader <file>   指定 preloader 文件路径
      --parttype <type>    指定分区类型
      --offset <n>         指定偏移地址
      --length <n>         指定长度
      --sector <n>         指定扇区号
      --sectors <n>        指定扇区数
      --verify             写入后校验
      --log <1-3>          日志级别：1=INFO（默认），2=DEBUG，3=TRACE（完整原始 hex dump）
      --quiet              静默模式（只输出错误和最终结果）
      --quiet_dump         静默 dump 模式（不打印进度条和 USB 读取日志）
      --usb_log            启用 USB 通信追踪日志（输出到 usb_debug.log）
      --log_file <FILE>    全部日志同步写入指定文件（追加模式）
      --patch_da           是否 patch DA（默认开启）
      --no_elevate         跳过管理员提权检查
      --mode <mode>        工作模式：brom（默认）、preloader、auto（预留）
      --da_x_speed <1-3>   DA 加载速度：1=完整协议、2=快速、3=极速（默认）
      --skip <parts>       rl 命令跳过的分区名（逗号分隔）
      --data-dir <dir>     指定数据目录（默认程序所在目录下的 data/）
      --language <LANG>    输出语言：en-US（默认）、zh-CN、fr-FR、ru-RU、es-ES、ar-SA
  -h, --help               显示帮助
  -V, --version            显示版本
"#;

const HELP_FR: &str = r#"mtkclient-rs
Outil de flashage bas niveau MTK — 87 chipsets pris en charge | Windows 10/11 x64

Usage : mtkclient-rs [OPTIONS] <COMMANDE> [ARGS]

Commandes :
  Opérations sur les partitions :
    printgpt                    Affiche la table GPT + infos eMMC + arborescence des
                                partitions dynamiques de super
    r <part> <file>             Lit une partition vers un fichier (détection dynamique)
      r boot boot.img           Lit la partition boot
      r boot1 boot1.bin         Lit eMMC boot1 (partition spéciale)
      r system system.img       Lit une partition dynamique depuis super automatiquement
      r super --dp system_b system_b.img  Lit une partition dynamique précise de super
      r gpt <dir>               Extrait les données GPT brutes vers un répertoire
      w gpt <file>              Réécrit les données GPT brutes (inverse de r gpt, eMMC)
    rl <dir> [--skip <...>]     Lit toutes les partitions vers un répertoire
    w <part> <file>             Écrit un fichier dans une partition
    wl <dir>                    Restaure toutes les partitions depuis un répertoire
    e <part>                    Efface une partition
  Informations :
    zyb get_build_prop [part]   Lit build.prop (défaut : partition system)
    slot show|a|b               Affiche / change le slot A/B
  Sécurité & déverrouillage :
    zyb seccfg unlock           Déverrouille le bootloader (seccfg V3/V4 + vbmeta désactivé)
    zyb seccfg lock             Verrouille le bootloader (seccfg V3/V4)
    zyb seccfg info [fichier]   Affiche l'état seccfg (hwtype, verrou, images)
    zyb oem unlock              Déverrouillage FRP OEM (patch du flag de la partition frp)
    zyb oem lock                Reverrouillage FRP OEM
    zyb vbmeta <0-3>            Patche vbmeta (0=vérification off, 1=on, 2/3=réservé)
    zyb erase_data              Efface données utilisateur + metadata (reset usine)
    frp                         Déverrouillage FRP OEM (alias de zyb oem unlock)
  Contrôle du redémarrage :
    reboot [mode] [--via way]   Redémarre l'appareil (défaut : system)
      reboot fastboot           Redémarre vers le bootloader (lk fastboot, mode BROM uniquement)
      reboot recovery           Redémarre en recovery
      reboot fastbootd          Redémarre en fastbootd (fastboot espace utilisateur)
      reboot meta               Redémarre en mode META
      --via misc|para|da|xml|preloader   Voie de redémarrage (défaut : misc)
  Divers :
    chain <goal>                Affiche la chaîne d'exploit planifiée pour un objectif
    plan <op>                   Affiche les chemins planifiés (>=2) et les raisons, hors ligne
                                (read|rp|write|peek|dump) — selon l'état de l'appareil
    avb <info|disable> <file>   avbtool hors ligne : analyse vbmeta / désactive la vérification
    fs_shell boot [file]        Navigue interactivement le ramdisk d'une image boot (cpio)
    fs_shell proinfo [file]     Inspecte interactivement la partition proinfo MTK (données NV)
    dumppreloader               Extrait le preloader depuis la RAM (exploit BROM)
    dumpbrom [fichier]          Extrait le BROM lui-même (128 Ko @ 0x0)
    genkey                      Génère une clé RPMB aléatoire de 32 octets (hex)
    rpmb info|init|read|write        Programmation de clé RPMB et accès à la partition
    adb                         Active ADB en mode DA et redémarre
    peek <addr> [size]          Lit la mémoire de l'appareil (4 octets par défaut), hex dump
    poke <addr> <hex>           Écrit des données dans la mémoire de l'appareil
    fs_shell                    Navigue interactivement dans le système de fichiers de super
    multi "<cmd1>;<cmd2>..."    Exécute plusieurs commandes en une session DA
    run <script.json>           Exécute des commandes depuis un script JSON
  help                          Affiche cette aide

Options :
      --da <file>          Chemin du fichier DA loader
      --da2 <file>         Chemin du fichier DA2
      --preloader <file>   Chemin du fichier preloader
      --parttype <type>    Type de partition
      --offset <n>         Adresse de décalage
      --length <n>         Longueur
      --sector <n>         Numéro de secteur
      --sectors <n>        Nombre de secteurs
      --verify             Vérifie après écriture
      --log <1-3>          Niveau de log : 1=INFO (défaut), 2=DEBUG, 3=TRACE (hex dump brut)
      --quiet              Mode silencieux (erreurs et résultats finaux uniquement)
      --quiet_dump         Mode dump silencieux (pas de barre de progression ni logs USB)
      --usb_log            Trace USB (vers usb_debug.log)
      --log_file <FICHIER> Duplique tous les logs vers FICHIER (ajout)
      --patch_da           Patche le DA (activé par défaut)
      --no_elevate         Ignore la vérification d'élévation administrateur
      --mode <mode>        Mode de travail : brom (défaut) | preloader | auto (réservé)
      --da_x_speed <1-3>   Vitesse de chargement DA : 1=protocole complet, 2=rapide, 3=max (défaut)
      --skip <parts>       Partitions à ignorer pour rl (séparées par des virgules)
      --data-dir <dir>     Répertoire de données (défaut : data/ à côté du programme)
      --language <LANG>    Langue de sortie : en-US (défaut), zh-CN, fr-FR, ru-RU, es-ES, ar-SA
  -h, --help               Affiche l'aide
  -V, --version            Affiche la version
"#;

const HELP_RU: &str = r#"mtkclient-rs
Низкоуровневый инструмент прошивки MTK — поддержка 87 чипсетов | Windows 10/11 x64

Использование: mtkclient-rs [ОПЦИИ] <КОМАНДА> [АРГУМЕНТЫ]

Команды:
  Работа с разделами:
    printgpt                    Печать таблицы GPT + информация eMMC + дерево динамических
                                разделов super
    r <part> <file>             Прочитать раздел в файл (автоопределение динамических)
      r boot boot.img           Прочитать раздел boot
      r boot1 boot1.bin         Прочитать eMMC boot1 (специальный раздел)
      r system system.img       Автоматически прочитать динамический раздел из super
      r super --dp system_b system_b.img  Прочитать указанный динамический раздел super
      r gpt <dir>               Выгрузить сырые данные GPT в каталог
      w gpt <file>              Записать сырые данные GPT обратно (обратно к r gpt, eMMC)
    rl <dir> [--skip <...>]     Прочитать все разделы в каталог (список пропуска через запятую)
    w <part> <file>             Записать файл в раздел
    wl <dir>                    Восстановить все разделы из каталога (.bin/.img)
    e <part>                    Стереть раздел
  Информация:
    zyb get_build_prop [part]   Прочитать build.prop (по умолчанию раздел system)
    slot show|a|b               Показать / переключить слот A/B
  Безопасность и разблокировка:
    zyb seccfg unlock           Разблокировать bootloader (seccfg V3/V4 + авто-отключение vbmeta)
    zyb seccfg lock             Заблокировать bootloader (seccfg V3/V4)
    zyb seccfg info [файл]      Показать состояние seccfg (hwtype, блокировка, образы)
    zyb oem unlock              FRP OEM разблокировка (патч флага раздела frp)
    zyb oem lock                FRP OEM блокировка
    zyb vbmeta <0-3>            Патч vbmeta (0=проверка выкл, 1=вкл, 2/3=резерв)
    zyb erase_data              Очистка пользовательских данных + metadata (сброс к заводским)
    frp                         FRP OEM разблокировка (синоним zyb oem unlock)
  Управление перезагрузкой:
    reboot [mode] [--via way]   Перезагрузить устройство (по умолчанию system)
      reboot fastboot           Перезагрузка в bootloader (lk fastboot, только режим BROM)
      reboot recovery           Перезагрузка в recovery
      reboot fastbootd          Перезагрузка в fastbootd (fastboot пространства пользователя)
      reboot meta               Перезагрузка в режим META
      --via misc|para|da|xml|preloader   Путь перезагрузки (по умолчанию misc)
  Прочее:
    chain <goal>                Показать запланированную цепочку эксплойтов для цели
    plan <op>                   Показать план путей (>=2) и причины, офлайн
                                (read|rp|write|peek|dump) — с учётом состояния устройства
    avb <info|disable> <file>   Автономный avbtool: разбор vbmeta / отключение проверки
    fs_shell boot [file]        Интерактивный просмотр ramdisk образа boot (cpio)
    fs_shell proinfo [file]     Интерактивный просмотр раздела proinfo MTK (данные NV)
    dumppreloader               Извлечь preloader из RAM (эксплойт BROM)
    dumpbrom [файл]             Извлечь сам BROM (128 КБ @ 0x0)
    genkey                      Сгенерировать случайный 32-байтовый RPMB-ключ (hex)
    rpmb info|init|read|write        Прошивка ключа RPMB и доступ к разделу
    adb                         Включить ADB в режиме DA и перезагрузить
    peek <addr> [size]          Прочитать память устройства (по умолчанию 4 байта), hex dump
    poke <addr> <hex>           Записать данные в память устройства
    fs_shell                    Интерактивный просмотр файловой системы раздела super
    multi "<cmd1>;<cmd2>..."    Несколько команд за одну DA-сессию
    run <script.json>           Выполнить команды из JSON-скрипта
  help                          Показать эту справку

Опции:
      --da <file>          Путь к файлу DA loader
      --da2 <file>         Путь к файлу DA2
      --preloader <file>   Путь к файлу preloader
      --parttype <type>    Тип раздела
      --offset <n>         Адрес смещения
      --length <n>         Длина
      --sector <n>         Номер сектора
      --sectors <n>        Количество секторов
      --verify             Проверка после записи
      --log <1-3>          Уровень лога: 1=INFO (по умолчанию), 2=DEBUG, 3=TRACE (hex dump)
      --quiet              Тихий режим (только ошибки и итоговые результаты)
      --quiet_dump         Тихий режим дампа (без прогресс-бара и логов чтения USB)
      --usb_log            Трассировка USB (в usb_debug.log)
      --log_file <ФАЙЛ>    Дублировать все логи в ФАЙЛ (дозапись)
      --patch_da           Патчить DA (по умолчанию включено)
      --no_elevate         Пропустить проверку повышения прав
      --mode <mode>        Режим работы: brom (по умолчанию) | preloader | auto (резерв)
      --da_x_speed <1-3>   Скорость загрузки DA: 1=полный протокол, 2=быстро, 3=макс (по умолчанию)
      --skip <parts>       Разделы для пропуска при rl (через запятую)
      --data-dir <dir>     Каталог данных (по умолчанию data/ рядом с программой)
      --language <LANG>    Язык вывода: en-US (по умолчанию), zh-CN, fr-FR, ru-RU, es-ES, ar-SA
  -h, --help               Показать справку
  -V, --version            Показать версию
"#;

const HELP_ES: &str = r#"mtkclient-rs
Herramienta de flasheo de bajo nivel MTK — 87 chipsets compatibles | Windows 10/11 x64

Uso: mtkclient-rs [OPCIONES] <COMANDO> [ARGS]

Comandos:
  Operaciones de particiones:
    printgpt                    Imprime la tabla GPT + info eMMC + árbol de particiones
                                dinámicas de super
    r <part> <file>             Lee una partición a un archivo (detecta dinámicas)
      r boot boot.img           Lee la partición boot
      r boot1 boot1.bin         Lee eMMC boot1 (partición especial)
      r system system.img       Lee una partición dinámica desde super automáticamente
      r super --dp system_b system_b.img  Lee una partición dinámica específica de super
      r gpt <dir>               Vuelca los datos GPT crudos a un directorio
      w gpt <file>              Escribe los datos GPT crudos (inverso de r gpt, eMMC)
    rl <dir> [--skip <...>]     Lee todas las particiones a un directorio (lista de omisión)
    w <part> <file>             Escribe un archivo en una partición
    wl <dir>                    Restaura todas las particiones desde un directorio
    e <part>                    Borra una partición
  Información:
    zyb get_build_prop [part]   Lee build.prop (por defecto: partición system)
    slot show|a|b               Muestra / cambia la ranura A/B
  Seguridad y desbloqueo:
    zyb seccfg unlock           Desbloquea el bootloader (seccfg V3/V4 + vbmeta desactivado)
    zyb seccfg lock             Bloquea el bootloader (seccfg V3/V4)
    zyb seccfg info [archivo]   Muestra el estado seccfg (hwtype, bloqueo, imágenes)
    zyb oem unlock              Desbloqueo FRP OEM (parchea el flag de la partición frp)
    zyb oem lock                Rebloqueo FRP OEM
    zyb vbmeta <0-3>            Parchea vbmeta (0=verificación off, 1=on, 2/3=reservado)
    zyb erase_data              Borra datos de usuario + metadata (restablecimiento de fábrica)
    frp                         Desbloqueo FRP OEM (alias de zyb oem unlock)
  Control de reinicio:
    reboot [mode] [--via way]   Reinicia el dispositivo (por defecto: system)
      reboot fastboot           Reinicia al bootloader (lk fastboot, solo modo BROM)
      reboot recovery           Reinicia a recovery
      reboot fastbootd          Reinicia a fastbootd (fastboot de espacio de usuario)
      reboot meta               Reinicia a modo META
      --via misc|para|da|xml|preloader   Vía de reinicio (por defecto: misc)
  Varios:
    chain <goal>                Muestra la cadena de exploit planificada para un objetivo
    plan <op>                   Muestra rutas planificadas (>=2) y motivos, sin conexión
                                (read|rp|write|peek|dump) — según el estado del dispositivo
    avb <info|disable> <file>   avbtool sin conexión: analiza vbmeta / desactiva la verificación
    fs_shell boot [file]        Navega interactivamente el ramdisk de una imagen boot (cpio)
    fs_shell proinfo [file]     Inspecciona interactivamente la partición proinfo MTK (NV)
    dumppreloader               Extrae el preloader de la RAM (exploit BROM)
    dumpbrom [archivo]          Extrae el propio BROM (128KB @ 0x0)
    genkey                      Genera una clave RPMB aleatoria de 32 bytes (hex)
    rpmb info|init|read|write        Programación de clave RPMB y acceso a la partición
    adb                         Activa ADB en modo DA y reinicia
    peek <addr> [size]          Lee memoria del dispositivo (4 bytes por defecto), hex dump
    poke <addr> <hex>           Escribe datos en la memoria del dispositivo
    fs_shell                    Navega interactivamente el sistema de archivos de super
    multi "<cmd1>;<cmd2>..."    Ejecuta varios comandos en una sesión DA
    run <script.json>           Ejecuta comandos desde un script JSON
  help                          Muestra esta ayuda

Opciones:
      --da <file>          Ruta del archivo DA loader
      --da2 <file>         Ruta del archivo DA2
      --preloader <file>   Ruta del archivo preloader
      --parttype <type>    Tipo de partición
      --offset <n>         Dirección de desplazamiento
      --length <n>         Longitud
      --sector <n>         Número de sector
      --sectors <n>        Cantidad de sectores
      --verify             Verifica tras escribir
      --log <1-3>          Nivel de log: 1=INFO (por defecto), 2=DEBUG, 3=TRACE (hex dump crudo)
      --quiet              Modo silencioso (solo errores y resultados finales)
      --quiet_dump         Modo dump silencioso (sin barra de progreso ni logs USB)
      --usb_log            Traza de comunicación USB (a usb_debug.log)
      --log_file <ARCHIVO> Duplica todos los logs a ARCHIVO (añadir)
      --patch_da           Parchea el DA (activado por defecto)
      --no_elevate         Omite la comprobación de elevación de administrador
      --mode <mode>        Modo de trabajo: brom (por defecto) | preloader | auto (reservado)
      --da_x_speed <1-3>   Velocidad de carga DA: 1=protocolo completo, 2=rápido, 3=máximo (defecto)
      --skip <parts>       Particiones a omitir en rl (separadas por comas)
      --data-dir <dir>     Directorio de datos (por defecto: data/ junto al programa)
      --language <LANG>    Idioma de salida: en-US (por defecto), zh-CN, fr-FR, ru-RU, es-ES, ar-SA
  -h, --help               Muestra la ayuda
  -V, --version            Muestra la versión
"#;

const HELP_AR: &str = r#"mtkclient-rs
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_aliases() {
        assert_eq!(parse("en-US"), Some(Lang::En));
        assert_eq!(parse("zh-CN"), Some(Lang::ZhCn));
        assert_eq!(parse("zh_TW"), None);
        assert_eq!(parse("fr"), Some(Lang::Fr));
        assert_eq!(parse("ru-RU"), Some(Lang::Ru));
        assert_eq!(parse("es"), Some(Lang::Es));
        assert_eq!(parse("ar-SA"), Some(Lang::Ar));
    }

    #[test]
    fn extract_language_anywhere() {
        let rest = extract_from_args(vec!["help".into(), "--language".into(), "zh-CN".into()]);
        assert_eq!(rest, vec!["help".to_string()]);
        assert_eq!(lang(), Lang::ZhCn);
        set(Lang::En);
    }

    #[test]
    fn catalog_is_complete() {
        for (key, texts) in MSGS {
            for (i, text) in texts.iter().enumerate() {
                assert!(
                    !text.trim().is_empty(),
                    "key={} lang_idx={} 翻译为空",
                    key,
                    i
                );
            }
            // 占位符数量必须跨语言一致，避免 tr() 替换出错
            let count = texts[0].matches('{').count();
            for (i, text) in texts.iter().enumerate() {
                assert_eq!(
                    text.matches('{').count(),
                    count,
                    "key={} lang_idx={} 占位符数量不一致",
                    key,
                    i
                );
            }
        }
    }

    #[test]
    fn log_keys_are_localized() {
        for key in ["log_da_loaded", "log_wait_device", "log_write_done"] {
            assert!(!t(key).is_empty(), "缺少 key: {}", key);
        }
        set(Lang::En);
        assert_eq!(t("log_da_loaded"), "DA loaded");
        set(Lang::ZhCn);
        assert_eq!(t("log_da_loaded"), "DA 加载完成");
        set(Lang::Fr);
        assert_eq!(t("log_da_loaded"), "DA chargé");
        set(Lang::En);
        assert_eq!(
            tr("log_write_done", &["1024", "boot"]).contains("boot"),
            true
        );
    }

    #[test]
    fn help_mentions_language_in_all_langs() {
        for l in [Lang::En, Lang::ZhCn, Lang::Fr, Lang::Ru, Lang::Es, Lang::Ar] {
            set(l);
            assert!(help().contains("mtkclient-rs"));
            assert!(help().contains("--language"));
        }
        set(Lang::En);
    }
}
