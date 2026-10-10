//! 文件落盘的小工具：给"写坏半份比写不上更糟"的配置类文件共用。

use std::io::Write;
use std::path::Path;

/// 原子写文本文件：先写**同目录**临时文件并 fsync，再 rename 覆盖目标
/// （Windows 的 rename 带 `MOVEFILE_REPLACE_EXISTING` 语义，可替换已存在文件）。
/// 直接 `fs::write` 原位覆盖，进程崩溃可能留下半份内容；rename 保证目标要么是
/// 旧内容、要么是新内容，崩溃最多留下一份孤儿临时文件。
///
/// 临时名含进程 id + 纳秒戳，并发写者互不踩踏——但真正的写互斥仍归调用方
/// （如壳对 gateway.json 的 ConfigWriteLock），这里只保证单次写入的原子性。
/// 父目录不存在则顺带创建（壳偏好首次落盘的场景）。
pub fn write_file_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".{stem}.tmp-{}-{nanos}", std::process::id()));
    // rename 前必须关掉句柄：Windows 对打开中的文件 rename 会失败。
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("toktol-fsutil-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_tmp_files() {
        let dir = scratch_dir("replace");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        write_file_atomic(&path, "old").unwrap();
        write_file_atomic(&path, "new, longer content").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "new, longer content"
        );

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "config.json")
            .collect();
        assert!(leftovers.is_empty(), "不应留下临时文件: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_creates_missing_parent_dirs() {
        let dir = scratch_dir("mkdir");
        let path = dir.join("nested").join("config.json");
        write_file_atomic(&path, "{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
