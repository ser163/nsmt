//! 进程 CPU/内存采样（跨平台）：调用系统 `ps` 解析。
//!
//! macOS / Linux 都支持：
//!   ps -o pid=,pcpu=,rss= -p <pid>
//! rss 单位 KB；pcpu 为近 15s 平均占用百分比。

/// 返回 `(cpu_pct, mem_mb)`。
pub fn ps_usage(pid: u32) -> std::io::Result<(f32, f32)> {
    let out = std::process::Command::new("ps")
        .args(["-o", "pid=,pcpu=,rss=", "-p", &pid.to_string()])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    let mut parts = line.split_whitespace();
    let _pid = parts.next();
    let cpu: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let rss_kb: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    Ok((cpu, rss_kb / 1024.0))
}
