//! Russian (ru-RU) message table and help text.

pub(super) const MESSAGES: &[&str] = &[
    // os_unsupported
    "Неподдерживаемая ОС: Windows {0}.{1} (сборка {2}). Требуется Windows 10 или 11.",
    // arch_unsupported
    "Ошибка: эта сборка поддерживает только 64-bit (x86_64).",
    // os_warning
    "Предупреждение: полностью протестировано только на Windows 10/11.",
    // get_updates
    "Обновления:",
    // free_tool_notice
    "Этот инструмент полностью бесплатен. Перепродажа, взлом или удаление информации об авторе запрещены.",
    // data_dir_missing
    "путь --data-dir не существует или не является каталогом: {0}",
    // elevate_request
    "[MAIN] Для установки драйвера WinUSB нужны права администратора; запрос повышения...",
    // elevate_failed
    "[MAIN] Ошибка повышения прав: {0}",
    // elevate_hint
    "[MAIN] Запустите от имени администратора (иначе переключение WinUSB будет недоступно)",
    // connected_preloader
    "Подключено (режим Preloader)",
    // connected_brom
    "Подключено (режим BROM)",
    // no_preloader_forced_dump
    "--preloader не указан; будет принудительно извлечён с устройства с перезаписью файла",
    // adb_rebooting
    "ADB включён; устройство перезагружается в систему",
    // unknown_command
    "Неизвестная команда: {0}",
    // usage_r_gpt
    "использование: mtkclient r gpt <dir>",
    // usage_rl
    "использование: mtkclient rl <dir>",
    // usage_wl
    "использование: mtkclient wl <dir>",
    // usage_w_gpt
    "использование: mtkclient w gpt <файл>",
    // read_gpt_failed
    "не удалось прочитать GPT: {0}",
    // gpt_unavailable
    "данные GPT недоступны",
    // parse_gpt_failed
    "не удалось разобрать GPT: {0}",
    // no_super_in_gpt
    "раздел super не найден в GPT",
    // log_wait_device
    "Ожидание устройства (BROM: Vol+ + Vol- + Power)",
    // log_auto_detect
    "[AUTO] Авторежим: ожидание устройства BROM или Preloader...",
    // log_brom_usb_found
    "[AUTO] Обнаружено устройство BROM WinUSB",
    // log_brom_com_ok
    "[AUTO] Рукопожатие по COM-порту BROM успешно",
    // log_da_loaded
    "DA загружен",
    // log_da_ext_ready
    "DA Extensions загружены, тип хранилища: {0}",
    // log_skip_existing
    "  Файл уже полон ({0} байт), чтение пропущено",
    // log_saved_to
    "  Сохранено в: {0}",
    // log_write_start
    "Запись {0} в раздел {1}...",
    // log_write_done
    "  Запись завершена: {0} байт записано в раздел {1}",
    // log_verify_ok
    "  Проверка пройдена ({0} байт)",
    // log_erase_start
    "Стирание {0}...",
    // log_erased
    "{0} стёрт",
    // log_format_start
    "Форматирование {0} (f2fs)...",
    // log_format_done
    "{0} отформатирован (f2fs)",
    // log_unlock_start
    "Разблокировка bootloader онлайн...",
    // log_unlock_ok
    "Bootloader разблокирован",
    // log_lock_start
    "Блокировка bootloader онлайн...",
    // log_lock_ok
    "Bootloader заблокирован",
    // log_rpmb_key_ok
    "RPMB-ключ записан и подтверждён эхом",
    // plan_usage
    "использование: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da ФАЙЛ]",
    // plan_unknown_op
    "неизвестная операция: {0} (доступно: printgpt/read/write/erase/seccfg/rpmb)",
    // plan_bad_mode
    "неверный --mode: {0} (brom/preloader/auto)",
];

pub(super) const HELP: &str = r#"mtkclient-rs
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
    unlock [--yes]              Разблокировка одной командой: seccfg + vbmeta
    fs_shell boot ls|cat|info   Просмотр ramdisk boot (только чтение)
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
    --sim <preset>              Полностью офлайн-запуск на виртуальном MTK
                                (v5-clean|v5-sla|v5-daa|v6|ufs-ab|flaky|unplug),
                                или --sim ./my.sim / --port mock://<preset>
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
