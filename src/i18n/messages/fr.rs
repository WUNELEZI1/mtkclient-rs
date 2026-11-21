//! French (fr-FR) message table and help texts.

pub(super) const MESSAGES: &[&str] = &[
    // os_unsupported
    "OS non pris en charge : Windows {0}.{1} (Build {2}). Windows 10 ou 11 requis.",
    // arch_unsupported
    "Erreur : cette build ne prend en charge que le 64 bits (x86_64).",
    // os_warning
    "Avertissement : testé uniquement sur Windows 10/11.",
    // get_updates
    "Mises à jour :",
    // free_tool_notice
    "Cet outil est entièrement gratuit. Revente, crack ou suppression des mentions d'auteur interdits.",
    // data_dir_missing
    "le chemin --data-dir n'existe pas ou n'est pas un répertoire : {0}",
    // elevate_request
    "[MAIN] Droits administrateur requis pour installer le pilote WinUSB ; élévation en cours...",
    // elevate_failed
    "[MAIN] Échec de l'élévation : {0}",
    // elevate_hint
    "[MAIN] Veuillez exécuter en tant qu'administrateur (sinon WinUSB ne pourra pas être activé)",
    // connected_preloader
    "Connecté (mode Preloader)",
    // connected_brom
    "Connecté (mode BROM)",
    // no_preloader_forced_dump
    "Aucun --preloader spécifié ; extraction forcée depuis l'appareil avec écrasement du fichier",
    // adb_rebooting
    "ADB activé ; l'appareil redémarre vers le système",
    // unknown_command
    "Commande inconnue : {0}",
    // usage_r_gpt
    "usage : mtkclient r gpt <dir>",
    // usage_rl
    "usage : mtkclient rl <dir>",
    // usage_wl
    "usage : mtkclient wl <dir>",
    // usage_w_gpt
    "usage : mtkclient w gpt <fichier>",
    // read_gpt_failed
    "échec de la lecture du GPT : {0}",
    // gpt_unavailable
    "données GPT indisponibles",
    // parse_gpt_failed
    "échec de l'analyse du GPT : {0}",
    // no_super_in_gpt
    "aucune partition super dans le GPT",
    // log_wait_device
    "En attente de l'appareil (BROM : Vol+ + Vol- + Power)",
    // log_auto_detect
    "[AUTO] Mode auto : attente d'un appareil BROM ou Preloader...",
    // log_brom_usb_found
    "[AUTO] Appareil BROM WinUSB détecté",
    // log_brom_com_ok
    "[AUTO] Poignée de main série BROM réussie",
    // log_da_loaded
    "DA chargé",
    // log_da_ext_ready
    "DA Extensions chargé, type de stockage : {0}",
    // log_skip_existing
    "  Fichier déjà complet ({0} octets), lecture ignorée",
    // log_saved_to
    "  Enregistré dans : {0}",
    // log_write_start
    "Écriture de {0} vers la partition {1}...",
    // log_write_done
    "  Écriture terminée : {0} octets écrits dans {1}",
    // log_verify_ok
    "  Vérification réussie ({0} octets)",
    // log_erase_start
    "Effacement de {0}...",
    // log_erased
    "{0} effacé",
    // log_format_start
    "Formatage de {0} (f2fs)...",
    // log_format_done
    "{0} formaté (f2fs)",
    // log_unlock_start
    "Déverrouillage du bootloader en ligne...",
    // log_unlock_ok
    "Bootloader déverrouillé",
    // log_lock_start
    "Verrouillage du bootloader en ligne...",
    // log_lock_ok
    "Bootloader verrouillé",
    // log_rpmb_key_ok
    "Clé RPMB écrite et vérifiée par écho",
    // plan_usage
    "usage : mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da FICHIER]",
    // plan_unknown_op
    "opération inconnue : {0} (disponibles : printgpt/read/write/erase/seccfg/rpmb)",
    // plan_bad_mode
    "--mode invalide : {0} (brom/preloader/auto)",
];

pub(super) const HELP: &str = r#"mtkclient-rs
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
    unlock [--yes]              Déverrouillage en une passe : seccfg + vbmeta
    fs_shell boot ls|cat|info   Parcourir le ramdisk boot (lecture seule)
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
    --sim <preset>              Exécuter hors ligne sur un appareil MTK simulé
                                (v5-clean|v5-sla|v5-daa|v6|ufs-ab|flaky|unplug),
                                ou --sim ./mon.sim / --port mock://<preset>
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
