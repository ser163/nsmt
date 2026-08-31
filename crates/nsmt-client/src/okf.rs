//! OKF (Open Knowledge Format v0.2) 支持
//!
//! 在 NSMT 共享目录之上提供 OKF 标准文件存储接口：
//!   yggd okf init [--root P]                初始化知识包（生成根 index.md）
//!   yggd okf new <rel-path> --type T ...    创建 concept（自动生成 frontmatter 模板）
//!   yggd okf validate [--root P]            校验 OKF 符合性（§11）
//!   yggd okf list [--type T] [--root P]     列出概念（frontmatter 摘要）
//!   yggd okf index [--root P]               生成/刷新各目录 index.md（§8）
//!   yggd okf show <rel-path> [--root P]     展示概念（frontmatter + 正文预览）
//!   yggd okf log <message> [--root P]       追加更新日志 log.md（§9）
//!
//! 目录约定：bundle 根默认取 NSMT_OKF_ROOT（未设置则用共享目录根）。
//! 与 NSMT 文件同步天然兼容：OKF 是纯文件格式，CAS/tree/锁/冲突处理直接覆盖。

use std::path::{Path, PathBuf};

/// OKF 保留文件名（§3.1），不可用作 concept
pub const RESERVED: [&str; 2] = ["index.md", "log.md"];

/// 解析 markdown 文件的 frontmatter 块（`---` 包裹的 YAML）。
/// 返回 (frontmatter, body)；无 frontmatter 或 YAML 解析失败返回 None。
pub fn parse_doc(content: &str) -> Option<(serde_yaml::Value, String)> {
    // 容忍 UTF-8 BOM（Windows 记事本）
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    let body = rest[end + 4..].trim_start_matches('\n').to_string();
    let v: serde_yaml::Value = serde_yaml::from_str(fm).ok()?;
    Some((v, body))
}

/// 解析 concept 文档，返回 frontmatter + body；失败说明不 OKF 符合。
pub fn load_concept(path: &Path) -> Option<(serde_yaml::Value, String)> {
    let raw = std::fs::read_to_string(path).ok()?;
    parse_doc(&raw)
}

/// 从 frontmatter 取字符串字段（如 type/title/description）
fn fm_str(v: &serde_yaml::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// 从 frontmatter 取 tags 列表（兼容 `[a, b]` 与 `- a\n- b`）
fn fm_tags(v: &serde_yaml::Value) -> Vec<String> {
    match v.get("tags") {
        Some(serde_yaml::Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        Some(serde_yaml::Value::String(s)) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// 当前 UTC 时间的 ISO 8601 字符串（YYYY-MM-DDTHH:MM:SSZ）
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 格雷戈里历换算
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 解析 bundle 根：NSMT_OKF_ROOT env 优先，否则共享目录根
pub fn bundle_root() -> PathBuf {
    if let Ok(p) = std::env::var("NSMT_OKF_ROOT") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    share_dir_fallback()
}

fn share_dir_fallback() -> PathBuf {
    std::env::var("NSMT_SHARE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join("nsmt_share"))
                .unwrap_or_else(|_| PathBuf::from("nsmt_share"))
        })
}

/// 收集 bundle 内全部 concept 文件（跳过保留文件），返回 (相对路径, 绝对路径)
fn collect_md_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    fn walk(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
                    let rel = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
                    if !RESERVED.contains(&rel.file_name().and_then(|x| x.to_str()).unwrap_or("")) {
                        out.push(rel);
                    }
                }
            }
        }
    }
    walk(root, root, &mut out);
    out.sort();
    out
}

// ─────────────────────────── 子命令分发 ───────────────────────────

/// `yggd okf <subcommand> ...` 入口
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let sub = args.get(3).map(|s| s.as_str()).unwrap_or("help");
    match sub {
        "init" => cmd_init(args),
        "new" => cmd_new(args),
        "validate" => cmd_validate(args),
        "list" => cmd_list(args),
        "index" => cmd_index(args),
        "show" => cmd_show(args),
        "log" => cmd_log(args),
        "help" | "--help" | "-h" => {
            println!("{}", USAGE);
            Ok(())
        }
        other => {
            anyhow::bail!("unknown okf subcommand `{other}`\n{USAGE}");
        }
    }
}

const USAGE: &str = r#"yggd okf — Open Knowledge Format (v0.2) bundle management on the NSMT share dir

Usage:
  yggd okf init [--root P]                   Initialize bundle (create root index.md)
  yggd okf new <rel-path> --type T [--title X] [--description D] [--tags a,b] [--status draft|stable|deprecated] [--root P]
                                             Create a concept document (frontmatter template)
  yggd okf validate [--root P]               Check OKF conformance (frontmatter + non-empty type)
  yggd okf list [--type T] [--root P]        List concepts with frontmatter summary
  yggd okf index [--root P]                  Generate/refresh index.md per directory
  yggd okf show <rel-path> [--root P]        Show a concept (frontmatter + body preview)
  yggd okf log <message> [--root P]          Append an entry to log.md
  yggd okf help                              Show this help

Bundle root: NSMT_OKF_ROOT env var, or the NSMT share dir (NSMT_SHARE_DIR).
Files are plain OKF bundles — synced, locked and conflict-handled by NSMT as usual."#;

// ─────────────────────────── 子命令实现 ───────────────────────────

/// `okf init [--root P]`：确保 bundle 目录结构，生成根 index.md（不存在时）
pub fn cmd_init(args: &[String]) -> anyhow::Result<()> {
    let (root, _) = parse_args(args, 4)?;
    std::fs::create_dir_all(&root)?;
    let idx = root.join("index.md");
    if !idx.exists() {
        std::fs::write(&idx, "# Knowledge Bundle\n\nInitialized by NSMT (OKF v0.2).\n")?;
        println!("okf: initialized bundle at {}", root.display());
    } else {
        println!("okf: bundle already exists at {}", root.display());
    }
    // 同步生成 index（若已有概念）
    generate_index(&root)?;
    Ok(())
}

/// `okf new <rel-path> --type T [--title X] [--description D] [--tags a,b] [--status S] [--root P]`
/// 创建 concept 文件（已存在则拒绝，避免覆盖）
pub fn cmd_new(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 4)?;
    let rel = opts.get("path").cloned().ok_or_else(|| anyhow::anyhow!("usage: yggd okf new <rel-path> --type T [--title X] ..."))?;
    let ftype = opts.get("type").cloned().ok_or_else(|| anyhow::anyhow!("--type is required (OKF §4.1)"))?;
    if RESERVED.contains(&rel.as_str()) {
        anyhow::bail!("`{rel}` is an OKF reserved filename");
    }
    if !rel.ends_with(".md") {
        anyhow::bail!("concept path must end with .md");
    }
    let target = root.join(&rel);
    if target.exists() {
        anyhow::bail!("concept already exists: {}", target.display());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let title = opts.get("title").cloned().unwrap_or_else(|| {
        Path::new(&rel).file_stem().and_then(|s| s.to_str()).unwrap_or(&ftype).to_string()
    });
    let desc = opts.get("description").cloned().unwrap_or_default();
    let tags = opts.get("tags").cloned().unwrap_or_default();
    let status = opts.get("status").cloned().unwrap_or_else(|| "draft".into());

    let mut fm = String::new();
    fm.push_str(&format!("type: {ftype}\n"));
    fm.push_str(&format!("title: {title}\n"));
    if !desc.is_empty() {
        fm.push_str(&format!("description: {desc}\n"));
    }
    if !tags.is_empty() {
        let list: Vec<String> = tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        fm.push_str(&format!("tags: [{}]\n", list.join(", ")));
    }
    fm.push_str(&format!("status: {status}\n"));
    fm.push_str(&format!("generated: {{ by: process:nsmt, at: {} }}\n", iso_now()));

    let body = format!("# {title}\n\n");
    let doc = format!("---\n{fm}---\n\n{body}");
    std::fs::write(&target, doc)?;
    println!("okf: created concept {rel} (type={ftype})");
    // 追加更新日志
    append_log(&root, &format!("**Creation**: Established [{}](/{})", title, rel.replace('\\', "/")))?;
    Ok(())
}

/// `okf validate [--root P]`：OKF §11 符合性校验
pub fn cmd_validate(args: &[String]) -> anyhow::Result<()> {
    let (root, _) = parse_args(args, 4)?;
    if !root.is_dir() {
        anyhow::bail!("bundle root not found: {} (run `yggd okf init`)", root.display());
    }
    let files = collect_md_files(&root);
    if files.is_empty() {
        println!("okf: no concept documents found under {}", root.display());
        return Ok(());
    }
    let mut errors = 0;
    for rel in &files {
        let p = root.join(rel);
        match load_concept(&p) {
            None => {
                errors += 1;
                println!("  ✗ {} — missing/unparseable frontmatter", rel.display());
            }
            Some((fm, _)) => match fm_str(&fm, "type") {
                Some(t) if !t.trim().is_empty() => {}
                _ => {
                    errors += 1;
                    println!("  ✗ {} — frontmatter has no non-empty `type`", rel.display());
                }
            },
        }
    }
    println!("okf: validated {} document(s), {} error(s)", files.len(), errors);
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// `okf list [--type T] [--root P]`：列出概念摘要
pub fn cmd_list(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 4)?;
    let type_filter = opts.get("type").cloned();
    if !root.is_dir() {
        println!("okf: no bundle at {} (run `yggd okf init`)", root.display());
        return Ok(());
    }
    let files = collect_md_files(&root);
    let mut shown = 0;
    for rel in &files {
        let p = root.join(rel);
        let Some((fm, _)) = load_concept(&p) else { continue };
        let ftype = fm_str(&fm, "type").unwrap_or_default();
        if let Some(f) = &type_filter {
            if ftype != *f {
                continue;
            }
        }
        let title = fm_str(&fm, "title").unwrap_or_else(|| rel.display().to_string());
        let desc = fm_str(&fm, "description").unwrap_or_default();
        let tags = fm_tags(&fm);
        let tag_s = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(",")) };
        println!("{:<18} {}{}", ftype, rel.display(), tag_s);
        if !desc.is_empty() {
            println!("{:<18}   {}", "", desc);
        }
        shown += 1;
    }
    println!("okf: {} concept(s) listed", shown);
    Ok(())
}

/// `okf index [--root P]`：为每个含概念的目录生成/刷新 index.md（§8）
pub fn cmd_index(args: &[String]) -> anyhow::Result<()> {
    let (root, _) = parse_args(args, 4)?;
    if !root.is_dir() {
        anyhow::bail!("bundle root not found: {}", root.display());
    }
    generate_index(&root)?;
    println!("okf: index.md refreshed under {}", root.display());
    Ok(())
}

fn generate_index(root: &Path) -> anyhow::Result<()> {
    let files = collect_md_files(root);
    // 按父目录分组
    let mut groups: std::collections::BTreeMap<String, Vec<PathBuf>> = Default::default();
    for rel in &files {
        let dir = rel.parent().map(|p| p.display().to_string()).unwrap_or_else(|| ".".into());
        groups.entry(dir).or_default().push(rel.clone());
    }
    let mut out = String::from("# Knowledge Bundle\n\n");
    for (dir, rels) in &groups {
        let heading = if dir == "." { "Root Concepts" } else { dir };
        out.push_str(&format!("## {heading}\n\n"));
        for rel in rels {
            let p = root.join(rel);
            let title = load_concept(&p)
                .and_then(|(fm, _)| fm_str(&fm, "title"))
                .unwrap_or_else(|| rel.display().to_string());
            let desc = load_concept(&p)
                .and_then(|(fm, _)| fm_str(&fm, "description"))
                .unwrap_or_default();
            let link = rel.display().to_string().replace('\\', "/");
            if desc.is_empty() {
                out.push_str(&format!("* [{title}]({link})\n"));
            } else {
                out.push_str(&format!("* [{title}]({link}) - {desc}\n"));
            }
        }
        out.push('\n');
    }
    if files.is_empty() {
        out.push_str("_No concepts yet. Run `yggd okf new <path> --type <Type>`._\n");
    }
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join("index.md"), out)?;
    Ok(())
}

/// `okf show <rel-path> [--root P]`：展示概念
pub fn cmd_show(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 4)?;
    let rel = opts.get("path").cloned().ok_or_else(|| anyhow::anyhow!("usage: yggd okf show <rel-path>"))?;
    let p = root.join(&rel);
    let Some((fm, body)) = load_concept(&p) else {
        anyhow::bail!("not a valid OKF concept: {} (missing frontmatter?)", p.display());
    };
    println!("path: {}", rel);
    println!("type: {}", fm_str(&fm, "type").unwrap_or_default());
    println!("title: {}", fm_str(&fm, "title").unwrap_or_default());
    println!("description: {}", fm_str(&fm, "description").unwrap_or_default());
    println!("status: {}", fm_str(&fm, "status").unwrap_or_else(|| "stable".into()));
    let tags = fm_tags(&fm);
    if !tags.is_empty() {
        println!("tags: {}", tags.join(", "));
    }
    println!("generated: {}", if fm.get("generated").is_some() { "present" } else { "absent" });
    println!("--- body (first 40 lines) ---");
    for line in body.lines().take(40) {
        println!("{line}");
    }
    Ok(())
}

/// `okf log <message> [--root P]`：追加更新日志（§9）
pub fn cmd_log(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 4)?;
    let msg = opts.get("path").cloned().ok_or_else(|| anyhow::anyhow!("usage: yggd okf log <message>"))?;
    append_log(&root, &msg)?;
    println!("okf: log entry appended");
    Ok(())
}

fn append_log(root: &Path, entry: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    let log_path = root.join("log.md");
    let today = iso_now()[..10].to_string();
    let mut content = if log_path.exists() {
        std::fs::read_to_string(&log_path)?
    } else {
        "# Directory Update Log\n\n".to_string()
    };
    // 若已有今天的日期段，插入条目；否则追加新日期段
    let marker = format!("## {today}\n");
    if content.contains(&marker) {
        // 在该日期段下追加（找日期段后第一个空行/下一个 ##）
        if let Some(idx) = content.find(&marker) {
            let seg = &content[idx + marker.len()..];
            let seg_end = seg.find("\n## ").map(|i| idx + marker.len() + i).unwrap_or(content.len());
            content.insert_str(seg_end, &format!("* {entry}\n"));
        }
    } else {
        content.push_str(&format!("\n{marker}* {entry}\n"));
    }
    std::fs::write(log_path, content)?;
    Ok(())
}

/// 解析 `yggd okf <sub> ...` 参数：返回 (root, key->value map)
/// 支持 `--key value` 与位置参数（path/message 记录为 "path"）
fn parse_args(args: &[String], start: usize) -> anyhow::Result<(PathBuf, std::collections::HashMap<String, String>)> {
    let mut opts: std::collections::HashMap<String, String> = Default::default();
    let mut positional = Vec::new();
    let mut i = start;
    while i < args.len() {
        let a = &args[i];
        if let Some(k) = a.strip_prefix("--") {
            let v = args.get(i + 1).cloned().unwrap_or_default();
            opts.insert(k.to_string(), v);
            i += 2;
        } else {
            positional.push(a.clone());
            i += 1;
        }
    }
    if let Some(p) = positional.first() {
        opts.insert("path".to_string(), p.clone());
    }
    let root = opts
        .get("root")
        .map(PathBuf::from)
        .unwrap_or_else(bundle_root);
    Ok((root, opts))
}
