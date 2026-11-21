//! Spanish (es-ES) message table and help texts.

pub(super) const MESSAGES: &[&str] = &[
    // os_unsupported
    "SO no compatible: Windows {0}.{1} (compilación {2}). Se requiere Windows 10 u 11.",
    // arch_unsupported
    "Error: esta compilación solo admite 64 bits (x86_64).",
    // os_warning
    "Advertencia: probado completamente solo en Windows 10/11.",
    // get_updates
    "Actualizaciones:",
    // free_tool_notice
    "Esta herramienta es totalmente gratuita. Prohibida su reventa, crack o eliminación de la autoría.",
    // data_dir_missing
    "la ruta --data-dir no existe o no es un directorio: {0}",
    // elevate_request
    "[MAIN] Se requieren derechos de administrador para instalar el controlador WinUSB; elevando...",
    // elevate_failed
    "[MAIN] Error de elevación: {0}",
    // elevate_hint
    "[MAIN] Ejecute como administrador (de lo contrario no se podrá cambiar a WinUSB)",
    // connected_preloader
    "Conectado (modo Preloader)",
    // connected_brom
    "Conectado (modo BROM)",
    // no_preloader_forced_dump
    "No se especificó --preloader; se extraerá del dispositivo sobrescribiendo el archivo",
    // adb_rebooting
    "ADB activado; el dispositivo reinicia al sistema",
    // unknown_command
    "Comando desconocido: {0}",
    // usage_r_gpt
    "uso: mtkclient r gpt <dir>",
    // usage_rl
    "uso: mtkclient rl <dir>",
    // usage_wl
    "uso: mtkclient wl <dir>",
    // usage_w_gpt
    "uso: mtkclient w gpt <archivo>",
    // read_gpt_failed
    "error al leer el GPT: {0}",
    // gpt_unavailable
    "datos GPT no disponibles",
    // parse_gpt_failed
    "error al analizar el GPT: {0}",
    // no_super_in_gpt
    "no se encontró la partición super en el GPT",
    // log_wait_device
    "Esperando el dispositivo (BROM: Vol+ + Vol- + Power)",
    // log_auto_detect
    "[AUTO] Modo automático: esperando un dispositivo BROM o Preloader...",
    // log_brom_usb_found
    "[AUTO] Dispositivo BROM WinUSB detectado",
    // log_brom_com_ok
    "[AUTO] Handshake serie BROM correcto",
    // log_da_loaded
    "DA cargado",
    // log_da_ext_ready
    "DA Extensions cargado, tipo de almacenamiento: {0}",
    // log_skip_existing
    "  Archivo ya completo ({0} bytes), se omite la lectura",
    // log_saved_to
    "  Guardado en: {0}",
    // log_write_start
    "Escribiendo {0} en la partición {1}...",
    // log_write_done
    "  Escritura completada: {0} bytes en la partición {1}",
    // log_verify_ok
    "  Verificación correcta ({0} bytes)",
    // log_erase_start
    "Borrando {0}...",
    // log_erased
    "{0} borrado",
    // log_format_start
    "Formateando {0} (f2fs)...",
    // log_format_done
    "{0} formateado (f2fs)",
    // log_unlock_start
    "Desbloqueando el bootloader en línea...",
    // log_unlock_ok
    "Bootloader desbloqueado",
    // log_lock_start
    "Bloqueando el bootloader en línea...",
    // log_lock_ok
    "Bootloader bloqueado",
    // log_rpmb_key_ok
    "Clave RPMB escrita y verificada por eco",
    // plan_usage
    "uso: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da ARCHIVO]",
    // plan_unknown_op
    "operación desconocida: {0} (disponibles: printgpt/read/write/erase/seccfg/rpmb)",
    // plan_bad_mode
    "--mode no válido: {0} (brom/preloader/auto)",
];

pub(super) const HELP: &str = r#"mtkclient-rs
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
    unlock [--yes]              Desbloqueo en un paso: seccfg + vbmeta
    fs_shell boot ls|cat|info   Explorar ramdisk boot (solo lectura)
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
    --sim <preset>              Ejecutar sin conexión contra un dispositivo MTK
                                simulado (v5-clean|v5-sla|v5-daa|v6|ufs-ab|flaky),
                                o --sim ./mi.sim / --port mock://<preset>
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
