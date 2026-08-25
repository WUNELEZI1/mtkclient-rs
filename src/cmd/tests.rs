//! cmd 模块的单元测试

use super::*;
use crate::error::AppError;

#[test]
fn transport_error_detection() {
    // 真实传输层失败 → 应判为需要重置会话
    assert!(
        is_transport_error(&AppError::Usb("device disconnected".into())),
        "AppError::Usb 应判为传输错误"
    );
    assert!(
        is_transport_error(&AppError::Protocol("read data: read err: timeout".into())),
        "USB 读取超时(含 read err/timeout)应判为传输错误"
    );
    assert!(
        is_transport_error(&AppError::Protocol("写入超时".into())),
        "写入超时(含 超时)应判为传输错误"
    );

    // 业务/命令/参数/文件/解析/安全错误 → 不应重置会话
    assert!(
        !is_transport_error(&AppError::Protocol(
            "用法: mtkclient slot show|a|b（未知槽位操作: shwo）".into()
        )),
        "命令拼写错误不应重置会话"
    );
    assert!(
        !is_transport_error(&AppError::Parse("解析 GPT 失败: ...".into())),
        "解析错误不应重置会话"
    );
    assert!(
        !is_transport_error(&AppError::Security("安全错误: ...".into())),
        "安全错误不应重置会话"
    );
    assert!(
        !is_transport_error(&AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "os error 2"
        ))),
        "文件不存在(os error 2)不应重置会话"
    );
    assert!(
        !is_transport_error(&AppError::Protocol("GPT 中未找到 super 分区".into())),
        "分区找不到不应重置会话"
    );
}

#[test]
fn active_read_resume_detects_sidecar_file() {
    let output = std::env::temp_dir().join(format!("cmd_active_resume_{}.img", std::process::id()));
    let output = output.to_string_lossy().to_string();

    // written=4096, file=4096 → 精确匹配，应该检测到
    std::fs::write(
        format!("{}.resume", output),
        "active_read=true\nwritten=4096\n",
    )
    .unwrap();
    std::fs::write(&output, vec![0u8; 4096]).unwrap();
    assert!(active_read_resume_exists(&[
        "boot_b".to_string(),
        output.clone()
    ]));

    // written=8192, file=4096 → BufWriter 未刷完，应该检测到（放宽校验）
    std::fs::write(
        format!("{}.resume", output),
        "active_read=true\nwritten=8192\n",
    )
    .unwrap();
    assert!(active_read_resume_exists(&[
        "boot_b".to_string(),
        output.clone()
    ]));

    // written=200MB, file=0 → 差距 200MB > 128MB 容差，应该不检测
    std::fs::write(
        format!("{}.resume", output),
        format!("active_read=true\nwritten={}\n", 200 * 1024 * 1024),
    )
    .unwrap();
    std::fs::write(&output, vec![0u8; 0]).unwrap();
    assert!(!active_read_resume_exists(&[
        "boot_b".to_string(),
        output.clone()
    ]));

    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_file(format!("{}.resume", output));
}

#[test]
fn pending_read_resume_in_dir_detects_active_only() {
    let dir = std::env::temp_dir().join(format!("pending_read_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);

    // 1) active_read=true → 应被检测
    let out1 = dir.join("super.img");
    std::fs::write(
        dir.join("super.img.resume"),
        "output=super.img\nsize=10737418240\nwritten=11403264\nactive_read=true\n",
    )
    .unwrap();

    // 2) active_read=false（读取出错）→ 不应被检测
    let out2 = dir.join("expdb.img");
    std::fs::write(
        dir.join("expdb.img.resume"),
        "output=expdb.img\nsize=10485760\nwritten=4096\nactive_read=false\n",
    )
    .unwrap();

    // 3) 非 resume 文件 → 忽略
    std::fs::write(dir.join("note.txt"), "hello").unwrap();

    let pending = pending_read_resume_in_dir(dir.to_str().unwrap());
    assert_eq!(pending.len(), 1, "只应检测到 active_read=true 的任务");
    assert_eq!(pending[0].output, "super.img");
    assert_eq!(pending[0].size, 10737418240);
    assert_eq!(pending[0].written, 11403264);

    let _ = std::fs::remove_file(out1);
    let _ = std::fs::remove_file(dir.join("super.img.resume"));
    let _ = std::fs::remove_file(out2);
    let _ = std::fs::remove_file(dir.join("expdb.img.resume"));
    let _ = std::fs::remove_file(dir.join("note.txt"));
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn is_transport_error_precise_by_variant() {
    use crate::error::AppError;
    use std::io;

    // 1) Usb 变体：精确命中，必须重置会话
    assert!(is_transport_error(&AppError::Usb("设备断连".into())));

    // 2) Parse/Security：即便消息含传输关键词也不应重置（回归：旧版按关键词误判）
    assert!(!is_transport_error(&AppError::Parse(
        "读取超时：分区表校验失败".into()
    )));
    assert!(!is_transport_error(&AppError::Security("解锁超时".into())));

    // 3) Protocol/Io：仍走关键词兜底（含超时/端点等，大小写不敏感）
    assert!(is_transport_error(&AppError::Protocol(
        "DA 端点错误 timeout".into()
    )));
    let io_err = AppError::Io(io::Error::new(io::ErrorKind::NotFound, "device not found"));
    assert!(is_transport_error(&io_err));

    // 4) 普通 Protocol 业务错误（无传输关键词）→ 不重置
    assert!(!is_transport_error(&AppError::Protocol(
        "未知命令: foo".into()
    )));
}
