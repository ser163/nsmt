//! OKF (Open Knowledge Format v0.2) 知识库管理 —— 多 bundle 支持
//!
//! 目录结构（知识库根 = NSMT_OKF_ROOT，默认 <NSMT_SHARE_DIR>/okf）：
//!   <root>/<library>/         每个子目录 = 一个 OKF 知识库（bundle）
//!     index.md               库根索引（frontmatter 仅带 okf_version，§8/§12）
//!     log.md                 更新日志（§9）
//!     <concept>.md           概念文档（frontmatter: type 必填，§4）
//!     <subdir>/...
//!
//! 命令：
//!   yggd okf libs new <name> [--title X] [--description D]    建库
//!   yggd okf libs list                                         库列表
//!   yggd okf libs show <name>                                  库详情
//!   yggd okf libs rm <name> --force                            删库
//!   yggd okf libs validate <name>                              库符合性校验（§11）
//!   yggd okf <lib> add <rel-path> --type T [--title X] [--description D] [--tags a,b] [--status S]
//!   yggd okf <lib> rm <rel-path>
//!   yggd okf <lib> edit <rel-path> [--type T] [--title X] [--description D] [--tags a,b] [--status S]
//!   yggd okf <lib> list [--type T]
//!   yggd okf <lib> show <rel-path>
//!   yggd okf <lib> index
//!   yggd okf <lib> log <message>
//!
//! 规范对齐（OKF v0.2 SPEC.md）：概念 ID=相对路径去 .md（§2）；保留文件名
//! index.md/log.md（§3.1）；type 为唯一必填（§4.1）；actor 约定 process:nsmt（§7）；
//! 根 index.md 可带 okf_version（§12）；edit 保留未知 frontmatter 字段（§4.1）；
//! 删除概念记录 **Deprecation** 日志（§9）。

use std::path::{Path, PathBuf};

/// OKF 保留文件名（§3.1）
pub const RESERVED: [&str; 2] = ["index.md", "log.md"];
/// 本实现声明的 OKF 版本
pub const OKF_VERSION: &str = "0.2";

// ─────────────────────────── 基础解析 ───────────────────────────

/// 解析 markdown 的 frontmatter 块（`---` 包裹 YAML）。返回 (frontmatter, body)。
pub fn parse_doc(content: &str) -> Option<(serde_yaml::Value, String)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    let body = rest[end + 4..].trim_start_matches('\n').to_string();
    let v: serde_yaml::Value = serde_yaml::from_str(fm).ok()?;
    Some((v, body))
}

pub fn load_concept(path: &Path) -> Option<(serde_yaml::Value, String)> {
    let raw = std::fs::read_to_string(path).ok()?;
    parse_doc(&raw)
}

fn fm_str(v: &serde_yaml::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

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

fn set_fm(v: &mut serde_yaml::Value, key: &str, val: serde_yaml::Value) {
    if let serde_yaml::Value::Mapping(m) = v {
        m.insert(serde_yaml::Value::String(key.to_string()), val);
    }
}

/// 当前 UTC ISO 8601（YYYY-MM-DDTHH:MM:SSZ）
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
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

// ─────────────────────────── 路径 ───────────────────────────

/// 知识库根目录（NSMT_OKF_ROOT，默认 <共享目录>/okf）
pub fn libs_root() -> PathBuf {
    if let Ok(p) = std::env::var("NSMT_OKF_ROOT") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    share_dir_fallback().join("okf")
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

/// 校验库名合法性（防路径穿越）
fn valid_lib_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-')
}

fn lib_dir(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

/// 收集 bundle 内 concept 文件（跳过保留文件与 .trash 回收站），返回相对路径列表
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
                    // 跳过 .trash 回收站
                    if e.file_name().to_string_lossy() == ".trash" {
                        continue;
                    }
                    walk(&p, root, out);
                } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
                    let rel = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
                    let fname = rel.file_name().and_then(|x| x.to_str()).unwrap_or("");
                    if !RESERVED.contains(&fname) {
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

/// 收集知识库（根下含 index.md 或任何 .md 的目录；跳过隐藏目录如 .trash）
fn collect_libs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = e.file_name().to_string_lossy().to_string();
                if valid_lib_name(&name) && !name.starts_with('.') && has_md(&p) {
                    out.push(name);
                }
            }
        }
    }
    out.sort();
    out
}

fn has_md(dir: &Path) -> bool {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && has_md(&p) {
                return true;
            }
            if p.extension().and_then(|x| x.to_str()) == Some("md") {
                return true;
            }
        }
    }
    false
}

// ─────────────────────────── 通用工具 ───────────────────────────

/// 解析 `yggd okf <lib|libs> <sub> ...` 之后的参数（start=5）
fn parse_args(args: &[String], start: usize) -> (PathBuf, std::collections::HashMap<String, String>) {
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
    (libs_root(), opts)
}

fn need(opts: &std::collections::HashMap<String, String>, key: &str, usage: &str) -> anyhow::Result<String> {
    opts.get(key).cloned().filter(|v| !v.is_empty()).ok_or_else(|| anyhow::anyhow!(usage.to_string()))
}

/// 追加 log.md 条目（§9：日期分组，最新在前）
fn append_log(root: &Path, entry: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    let log_path = root.join("log.md");
    let today = &iso_now()[..10];
    let mut content = if log_path.exists() {
        std::fs::read_to_string(&log_path)?
    } else {
        "# Directory Update Log\n\n".to_string()
    };
    let marker = format!("## {today}\n");
    if content.contains(&marker) {
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

/// 生成 index.md（§8：index.md 无 frontmatter，出现在任何目录）。
/// - 每个直接含概念的目录生成 index.md，条目链接相对该目录（relative-url）
/// - bundle 根 index.md 始终生成：列出子目录 + 根级概念（progressive disclosure）
fn generate_index(root: &Path) -> anyhow::Result<()> {
    let files = collect_md_files(root);
    // 分组：根级概念 vs 各子目录概念
    let mut root_concepts: Vec<PathBuf> = Vec::new();
    let mut dir_groups: std::collections::BTreeMap<String, Vec<PathBuf>> = Default::default();
    for rel in &files {
        let parent = rel.parent();
        let is_root = parent.is_none() || parent == Some(Path::new(""));
        if is_root {
            root_concepts.push(rel.clone());
        } else {
            let dir = parent.unwrap().display().to_string().replace('\\', "/");
            dir_groups.entry(dir).or_default().push(rel.clone());
        }
    }
    // 子目录 index.md（相对链接）
    for (dir, rels) in &dir_groups {
        let mut out = String::from("# Concepts\n\n");
        for rel in rels {
            let p = root.join(rel);
            let title = load_concept(&p)
                .and_then(|(fm, _)| fm_str(&fm, "title"))
                .unwrap_or_else(|| rel.display().to_string());
            let desc = load_concept(&p)
                .and_then(|(fm, _)| fm_str(&fm, "description"))
                .unwrap_or_default();
            // §8：子目录 index.md 的链接相对该子目录
            let rel_s = rel.display().to_string().replace('\\', "/");
            let link = rel_s.strip_prefix(&format!("{dir}/")).unwrap_or(&rel_s).to_string();
            if desc.is_empty() {
                out.push_str(&format!("* [{title}]({link})\n"));
            } else {
                out.push_str(&format!("* [{title}]({link}) - {desc}\n"));
            }
        }
        out.push('\n');
        let idx_path = root.join(&dir).join("index.md");
        std::fs::create_dir_all(idx_path.parent().unwrap_or(root))?;
        std::fs::write(&idx_path, out)?;
    }
    // 根 index.md：子目录条目（§8 支持）+ 根级概念
    let mut out = String::from("# Knowledge Bundle\n\n");
    for dir in dir_groups.keys() {
        out.push_str(&format!("* [{dir}]({dir}/)\n"));
    }
    for rel in &root_concepts {
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
    if files.is_empty() {
        out.push_str("_No concepts yet. Run `yggd okf <lib> add <path> --type <Type>`._\n");
    }
    out.push('\n');
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join("index.md"), out)?;
    Ok(())
}

/// 校验 bundle 符合性（§11）。返回错误数。
fn validate_bundle(root: &Path) -> anyhow::Result<(usize, usize)> {
    if !root.is_dir() {
        anyhow::bail!("bundle root not found: {}", root.display());
    }
    let files = collect_md_files(root);
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
    Ok((files.len(), errors))
}

/// 校验 bundle（--lint 模式，对标生态校验器 okft）。
/// 在 §11 基础上增加：status 枚举（§5.4）、ISO 8601 时间（§5）、
/// index.md/log.md 无 frontmatter（§8/§9）、正文 markdown 链接可解析（§6.1）。
/// 返回 (文档数, 错误数, 警告数)。链接不可解析记 warning（规范 §6.1 允许断链）。
fn validate_bundle_lint(root: &Path) -> anyhow::Result<(usize, usize, usize)> {
    if !root.is_dir() {
        anyhow::bail!("bundle root not found: {}", root.display());
    }
    let files = collect_md_files(root);
    let mut errors = 0;
    let mut warnings = 0;

    // 保留文件检查：index.md/log.md 不得带 frontmatter（§8/§9）
    for reserved in RESERVED {
        let p = root.join(reserved);
        if p.exists() && parse_doc(&std::fs::read_to_string(&p)?).is_some() {
            errors += 1;
            println!("  ✗ {reserved} — reserved file must not have frontmatter (§8/§9)");
        }
    }

    for rel in &files {
        let p = root.join(rel);
        let Some((fm, body)) = load_concept(&p) else {
            errors += 1;
            println!("  ✗ {} — missing/unparseable frontmatter", rel.display());
            continue;
        };
        // type 必填（§11）
        match fm_str(&fm, "type") {
            Some(t) if !t.trim().is_empty() => {}
            _ => {
                errors += 1;
                println!("  ✗ {} — frontmatter has no non-empty `type`", rel.display());
            }
        }
        // status 枚举（§5.4）
        if let Some(s) = fm_str(&fm, "status") {
            if !["draft", "stable", "deprecated"].contains(&s.as_str()) {
                errors += 1;
                println!("  ✗ {} — invalid status `{s}` (§5.4)", rel.display());
            }
        }
        // ISO 8601 时间字段（§5）：generated.at / stale_after / verified[].at
        check_iso_field(&fm, "stale_after", rel, &mut errors);
        if let Some(g) = fm.get("generated") {
            if let Some(at) = g.get("at").and_then(|v| v.as_str()) {
                if !is_iso8601(at) {
                    errors += 1;
                    println!("  ✗ {} — generated.at `{at}` is not ISO 8601 UTC (§5)", rel.display());
                }
            }
        }
        if let Some(v) = fm.get("verified") {
            // 单映射或列表
            let entries: Vec<&serde_yaml::Value> = match v {
                serde_yaml::Value::Sequence(s) => s.iter().collect(),
                other => std::iter::once(other).collect(),
            };
            for e in entries {
                if let Some(at) = e.get("at").and_then(|x| x.as_str()) {
                    if !is_iso8601(at) {
                        errors += 1;
                        println!("  ✗ {} — verified.at `{at}` is not ISO 8601 UTC (§5)", rel.display());
                    }
                }
            }
        }
        // 正文 markdown 链接可解析性（§6.1；断链记 warning 不记 error）
        let doc_dir = p.parent().unwrap_or(root);
        for link in extract_md_links(&body) {
            if is_external_link(&link) {
                continue;
            }
            let target = resolve_link(root, doc_dir, &link);
            if !target.exists() {
                warnings += 1;
                println!("  ⚠ {} — link `{link}` does not resolve in bundle (§6.1)", rel.display());
            }
        }
    }
    Ok((files.len(), errors, warnings))
}

/// ISO 8601 基本格式检查：YYYY-MM-DDTHH:MM:SSZ（或带偏移 +HH:MM）
fn is_iso8601(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 {
        return false;
    }
    let digit = |i: usize| b.get(i).map_or(false, |c| c.is_ascii_digit());
    digit(0) && digit(1) && digit(2) && digit(3)
        && b.get(4) == Some(&b'-')
        && digit(5) && digit(6)
        && b.get(7) == Some(&b'-')
        && digit(8) && digit(9)
        && b.get(10) == Some(&b'T')
        && digit(11) && digit(12)
        && b.get(13) == Some(&b':')
        && digit(14) && digit(15)
        && b.get(16) == Some(&b':')
        && digit(17) && digit(18)
        && (b.get(19) == Some(&b'Z') || (b.get(19) == Some(&b'+') || b.get(19) == Some(&b'-')))
}

fn check_iso_field(fm: &serde_yaml::Value, key: &str, rel: &Path, errors: &mut usize) {
    if let Some(v) = fm.get(key).and_then(|x| x.as_str()) {
        if !is_iso8601(v) {
            *errors += 1;
            println!("  ✗ {} — {key} `{v}` is not ISO 8601 UTC (§5)", rel.display());
        }
    }
}

/// 提取 markdown 链接目标（`[text](target)`）
fn extract_md_links(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // 找匹配的 ]
            if let Some(close) = body[i + 1..].find(']') {
                let after = i + 1 + close + 1;
                if bytes.get(after) == Some(&b'(') {
                    if let Some(paren_end) = body[after + 1..].find(')') {
                        let target = &body[after + 1..after + 1 + paren_end];
                        // 忽略带 title 形式 [t](url "title") 与空目标
                        let target = target.split(' ').next().unwrap_or(target);
                        if !target.is_empty() {
                            out.push(target.to_string());
                        }
                        i = after + 1 + paren_end + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

fn is_external_link(link: &str) -> bool {
    link.starts_with("http://")
        || link.starts_with("https://")
        || link.starts_with('#')
        || link.starts_with("mailto:")
        || link.starts_with("data:")
}

/// 解析链接到 bundle 内绝对路径：/ 开头=bundle 根相对；否则相对文档所在目录
fn resolve_link(root: &Path, doc_dir: &Path, link: &str) -> PathBuf {
    if let Some(p) = link.strip_prefix('/') {
        root.join(p)
    } else {
        doc_dir.join(link)
    }
}

// ─────────────────────────── 知识库管理（libs） ───────────────────────────

/// `yggd okf libs new <name> [--title X] [--description D]`
fn cmd_lib_new(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 5);
    let name = need(&opts, "path", "usage: yggd okf libs new <name> [--title X] [--description D]")?;
    if !valid_lib_name(&name) {
        anyhow::bail!("invalid library name `{name}` (use [a-z0-9._-], max 63 chars)");
    }
    let dir = lib_dir(&root, &name);
    if dir.exists() {
        anyhow::bail!("library already exists: {}", dir.display());
    }
    std::fs::create_dir_all(&dir)?;
    // 根 index.md：无 frontmatter（§8）；标题/描述可放入正文顶部
    let mut idx = String::from("# Knowledge Bundle\n\n");
    if let Some(t) = opts.get("title") {
        if !t.is_empty() {
            idx.push_str(&format!("> {t}\n\n"));
        }
    }
    if let Some(d) = opts.get("description") {
        if !d.is_empty() {
            idx.push_str(&format!("> {d}\n\n"));
        }
    }
    idx.push_str("_Empty library. Run `yggd okf <lib> add <path> --type <Type>`._\n");
    std::fs::write(dir.join("index.md"), idx)?;
    std::fs::write(dir.join("log.md"), "# Directory Update Log\n\n")?;
    println!("okf: library `{name}` created at {}", dir.display());
    Ok(())
}

/// `yggd okf libs list`
fn cmd_lib_list(args: &[String]) -> anyhow::Result<()> {
    let (root, _) = parse_args(args, 5);
    if !root.is_dir() {
        println!("okf: no libraries (root {} not found; run `yggd okf libs new <name>`)", root.display());
        return Ok(());
    }
    let libs = collect_libs(&root);
    if libs.is_empty() {
        println!("okf: no libraries under {} (run `yggd okf libs new <name>`)", root.display());
        return Ok(());
    }
    println!("{:<20} {:>8}  {}", "library", "concepts", "index");
    println!("{}", "-".repeat(60));
    for name in &libs {
        let dir = lib_dir(&root, name);
        let n = collect_md_files(&dir).len();
        let summary = load_concept(&dir.join("index.md"))
            .and_then(|(fm, _)| fm_str(&fm, "title"))
            .unwrap_or_default();
        println!("{:<20} {:>8}  {}", name, n, summary);
    }
    Ok(())
}

/// `yggd okf libs show <name>`
fn cmd_lib_show(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 5);
    let name = need(&opts, "path", "usage: yggd okf libs show <name>")?;
    let dir = lib_dir(&root, &name);
    if !dir.is_dir() {
        anyhow::bail!("library `{name}` not found");
    }
    let concepts = collect_md_files(&dir);
    let mut types: std::collections::BTreeMap<String, usize> = Default::default();
    for rel in &concepts {
        let t = load_concept(&dir.join(rel)).and_then(|(fm, _)| fm_str(&fm, "type")).unwrap_or_default();
        *types.entry(t).or_default() += 1;
    }
    println!("library: {name}");
    println!("path: {}", dir.display());
    println!("concepts: {} (index.md/log.md excluded)", concepts.len());
    for (t, n) in &types {
        println!("  {t}: {n}");
    }
    // 最近 log 条目
    let log_path = dir.join("log.md");
    if let Ok(raw) = std::fs::read_to_string(&log_path) {
        let entries: Vec<&str> = raw.lines().filter(|l| l.starts_with("* ")).collect();
        println!("log entries: {} (latest: {})", entries.len(), entries.last().unwrap_or(&""));
    }
    Ok(())
}

/// `yggd okf libs rm <name> --force`
fn cmd_lib_rm(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 5);
    let name = need(&opts, "path", "usage: yggd okf libs rm <name> --force")?;
    if opts.get("force").map(|v| v == "1" || v == "true").unwrap_or(false) != true
        && !args.iter().any(|a| a == "--force")
    {
        anyhow::bail!("refusing to remove library `{name}` without --force");
    }
    let dir = lib_dir(&root, &name);
    if !dir.is_dir() {
        anyhow::bail!("library `{name}` not found");
    }
    std::fs::remove_dir_all(&dir)?;
    println!("okf: library `{name}` removed");
    Ok(())
}

/// `yggd okf libs validate <name> [--lint]`
fn cmd_lib_validate(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 5);
    let name = need(&opts, "path", "usage: yggd okf libs validate <name> [--lint]")?;
    let dir = lib_dir(&root, &name);
    let lint = args.iter().any(|a| a == "--lint");
    if lint {
        let (total, errors, warnings) = validate_bundle_lint(&dir)?;
        println!("okf: linted library `{name}` — {total} document(s), {errors} error(s), {warnings} warning(s)");
        if errors > 0 {
            std::process::exit(1);
        }
    } else {
        let (total, errors) = validate_bundle(&dir)?;
        println!("okf: validated library `{name}` — {total} document(s), {errors} error(s)");
        if errors > 0 {
            std::process::exit(1);
        }
    }
    Ok(())
}

// ─────────────────────────── 库内概念 CRUD ───────────────────────────

fn resolve_lib(args: &[String]) -> anyhow::Result<(PathBuf, std::collections::HashMap<String, String>)> {
    let lib_name = args.get(3).cloned().unwrap_or_default();
    if !valid_lib_name(&lib_name) {
        anyhow::bail!("invalid library name `{lib_name}`");
    }
    let root = libs_root();
    let dir = lib_dir(&root, &lib_name);
    if !dir.is_dir() {
        anyhow::bail!("library `{lib_name}` not found (run `yggd okf libs new {lib_name}`)");
    }
    let (_, opts) = parse_args(args, 5);
    Ok((dir, opts))
}

/// `yggd okf <lib> add <rel-path> --type T [--title X] [--description D] [--tags a,b] [--status S]`
fn cmd_add(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let rel = need(&opts, "path", "usage: yggd okf <lib> add <rel-path> --type T [--title X] ...")?;
    let ftype = need(&opts, "type", "--type is required (OKF §4.1)")?;
    if RESERVED.contains(&rel.as_str()) {
        anyhow::bail!("`{rel}` is an OKF reserved filename (§3.1)");
    }
    if !rel.ends_with(".md") {
        anyhow::bail!("concept path must end with .md");
    }
    let target = dir.join(&rel);
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
    // §5.4: absent `status` ⇒ stable（规范默认），显式传 draft 标记未审阅
    let status = opts.get("status").cloned().unwrap_or_else(|| "stable".into());
    if !["draft", "stable", "deprecated"].contains(&status.as_str()) {
        anyhow::bail!("invalid status `{status}` (draft | stable | deprecated, §5.4)");
    }
    let resource = opts.get("resource").cloned().unwrap_or_default(); // §4.1 推荐字段

    let mut fm = format!("type: {ftype}\n");
    fm.push_str(&format!("title: {title}\n"));
    if !desc.is_empty() {
        fm.push_str(&format!("description: {desc}\n"));
    }
    if !resource.is_empty() {
        fm.push_str(&format!("resource: {resource}\n"));
    }
    if !tags.is_empty() {
        let list: Vec<String> = tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        fm.push_str(&format!("tags: [{}]\n", list.join(", ")));
    }
    fm.push_str(&format!("status: {status}\n"));
    fm.push_str(&format!("generated: {{ by: process:nsmt, at: {} }}\n", iso_now()));

    let doc = format!("---\n{fm}---\n\n# {title}\n\n");
    std::fs::write(&target, doc)?;
    append_log(&dir, &format!("**Creation**: Established [{}](/{})", title, rel.replace('\\', "/")))?;
    println!("okf: concept created {rel} (type={ftype})");
    Ok(())
}

/// `yggd okf <lib> rm <rel-path>` — 移入库内 .trash/ 回收站（可 restore），log.md 记 **Deprecation**
fn cmd_rm(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let rel = need(&opts, "path", "usage: yggd okf <lib> rm <rel-path>")?;
    if RESERVED.contains(&rel.as_str()) {
        anyhow::bail!("`{rel}` is a reserved filename and cannot be removed via concept ops");
    }
    let target = dir.join(&rel);
    if !target.exists() {
        anyhow::bail!("concept not found: {}", target.display());
    }
    let trash = dir.join(".trash").join(&rel);
    if let Some(parent) = trash.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&target, &trash)?;
    append_log(&dir, &format!("**Deprecation**: Removed [{}](/{})", rel, rel.replace('\\', "/")))?;
    println!("okf: concept moved to trash {rel} (restore with `okf <lib> restore {rel}`)");
    Ok(())
}

/// `yggd okf <lib> restore <rel-path>` — 从 .trash/ 恢复概念
fn cmd_restore(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let rel = need(&opts, "path", "usage: yggd okf <lib> restore <rel-path>")?;
    let trash = dir.join(".trash").join(&rel);
    if !trash.exists() {
        anyhow::bail!("not in trash: {rel} (path: {})", trash.display());
    }
    let target = dir.join(&rel);
    if target.exists() {
        anyhow::bail!("target already exists: {}", target.display());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&trash, &target)?;
    append_log(&dir, &format!("**Update**: Restored [{}](/{})", rel, rel.replace('\\', "/")))?;
    println!("okf: concept restored {rel}");
    Ok(())
}

/// `yggd okf <lib> edit <rel-path> [--type T] [--title X] [--description D] [--tags a,b] [--status S] [--stale-after ISO]`
/// 字段级行编辑：只替换/插入目标 key 的行，保留其余 frontmatter 原文（注释、缩进、顺序），
/// 保留未知字段（§4.1），更新 generated.at 并保留 generated.by（§5.2）。
fn cmd_edit(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let rel = need(&opts, "path", "usage: yggd okf <lib> edit <rel-path> [--title X] ...")?;
    let target = dir.join(&rel);
    let raw = std::fs::read_to_string(&target).map_err(|_| anyhow::anyhow!("concept not found: {}", target.display()))?;
    let (fm, body) = parse_doc(&raw).ok_or_else(|| anyhow::anyhow!("not a valid OKF concept: {}", target.display()))?;

    // 拆出原始 frontmatter 文本（不含首尾 ---）
    let inner = raw.strip_prefix("---").ok_or_else(|| anyhow::anyhow!("no frontmatter"))?;
    let fm_raw = inner.split("\n---").next().ok_or_else(|| anyhow::anyhow!("no closing ---"))?;
    let fm_raw = fm_raw.trim_start_matches('\n');
    let mut lines: Vec<String> = fm_raw.lines().map(|l| l.to_string()).collect();

    // 待写入的字段变更：(key, value 文本)
    let mut changes: Vec<(&str, String)> = Vec::new();

    if let Some(t) = opts.get("type") {
        if !t.is_empty() {
            changes.push(("type", t.clone()));
        }
    }
    if let Some(t) = opts.get("title") {
        if !t.is_empty() {
            changes.push(("title", t.clone()));
        }
    }
    if let Some(d) = opts.get("description") {
        if !d.is_empty() {
            changes.push(("description", d.clone()));
        }
    }
    if let Some(r) = opts.get("resource") {
        if !r.is_empty() {
            changes.push(("resource", r.clone()));
        }
    }
    if let Some(t) = opts.get("tags") {
        if !t.is_empty() {
            let list: Vec<String> = t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            changes.push(("tags", format!("[{}]", list.join(", "))));
        }
    }
    if let Some(s) = opts.get("status") {
        if !s.is_empty() {
            if !["draft", "stable", "deprecated"].contains(&s.as_str()) {
                anyhow::bail!("invalid status `{s}` (draft | stable | deprecated, §5.4)");
            }
            changes.push(("status", s.clone()));
        }
    }
    if let Some(st) = opts.get("stale-after") {
        if !st.is_empty() {
            changes.push(("stale_after", st.clone()));
        }
    }
    // generated.at 更新（保留 generated.by）——整行替换为内联格式
    let by = fm
        .get("generated")
        .and_then(|g| g.get("by"))
        .and_then(|v| v.as_str())
        .unwrap_or("process:nsmt");
    changes.push(("generated", format!("{{ by: {by}, at: {} }}", iso_now())));

    for (key, val) in &changes {
        let prefix = format!("{key}:");
        if let Some(idx) = lines.iter().position(|l| l.trim_start().starts_with(&prefix)) {
            // 保留行首缩进与行尾注释（# ...）
            let line = &lines[idx];
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            let comment = line.split_once('#').map(|(_, c)| format!("#{c}")).unwrap_or_default();
            lines[idx] = format!("{indent}{prefix} {val}{}", comment.trim_end());
        } else {
            lines.push(format!("{key}: {val}"));
        }
    }

    std::fs::write(&target, format!("---\n{}\n---\n\n{}", lines.join("\n"), body))?;
    append_log(&dir, &format!("**Update**: Edited [{}](/{})", rel, rel.replace('\\', "/")))?;
    println!("okf: concept updated {rel}");
    Ok(())
}

/// `yggd okf <lib> list [--type T]`
fn cmd_list(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let type_filter = opts.get("type").cloned();
    let files = collect_md_files(&dir);
    let mut shown = 0;
    for rel in &files {
        let Some((fm, _)) = load_concept(&dir.join(rel)) else { continue };
        let ftype = fm_str(&fm, "type").unwrap_or_default();
        if let Some(f) = &type_filter {
            if ftype != *f {
                continue;
            }
        }
        let desc = fm_str(&fm, "description").unwrap_or_default();
        let tags = fm_tags(&fm);
        let tag_s = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(",")) };
        // 概念 ID = 相对路径去 .md（§2）
        let cid = rel.with_extension("").display().to_string().replace('\\', "/");
        println!("{:<18} {}{}", ftype, cid, tag_s);
        if !desc.is_empty() {
            println!("{:<18}   {}", "", desc);
        }
        shown += 1;
    }
    println!("okf: {} concept(s) in library", shown);
    Ok(())
}

/// `yggd okf <lib> show <rel-path>`
fn cmd_show(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let rel = need(&opts, "path", "usage: yggd okf <lib> show <rel-path>")?;
    let p = dir.join(&rel);
    let Some((fm, body)) = load_concept(&p) else {
        anyhow::bail!("not a valid OKF concept: {} (missing frontmatter?)", p.display());
    };
    println!("concept: {}", rel.replace('\\', "/"));
    println!("type: {}", fm_str(&fm, "type").unwrap_or_default());
    println!("title: {}", fm_str(&fm, "title").unwrap_or_default());
    println!("description: {}", fm_str(&fm, "description").unwrap_or_default());
    println!("status: {}", fm_str(&fm, "status").unwrap_or_else(|| "stable".into()));
    let tags = fm_tags(&fm);
    if !tags.is_empty() {
        println!("tags: {}", tags.join(", "));
    }
    if let Some(g) = fm.get("generated") {
        println!("generated: {}", g.as_str().unwrap_or("present"));
    }
    println!("--- body (first 40 lines) ---");
    for line in body.lines().take(40) {
        println!("{line}");
    }
    Ok(())
}

/// `yggd okf <lib> index`
fn cmd_index(args: &[String]) -> anyhow::Result<()> {
    let (dir, _) = resolve_lib(args)?;
    generate_index(&dir)?;
    println!("okf: index.md refreshed for library");
    Ok(())
}

/// `yggd okf <lib> log <message>`
fn cmd_log(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let msg = need(&opts, "path", "usage: yggd okf <lib> log <message>")?;
    append_log(&dir, &msg)?;
    println!("okf: log entry appended");
    Ok(())
}

/// `yggd okf <lib> search <keyword>` — 在库内全部概念（frontmatter + 正文）做大小写不敏感检索
fn cmd_search(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let kw = need(&opts, "path", "usage: yggd okf <lib> search <keyword>")?;
    let kw_l = kw.to_lowercase();
    let files = collect_md_files(&dir);
    let mut hits = 0;
    for rel in &files {
        let p = dir.join(rel);
        let Some((fm, body)) = load_concept(&p) else { continue };
        let ftype = fm_str(&fm, "type").unwrap_or_default();
        let title = fm_str(&fm, "title").unwrap_or_else(|| rel.display().to_string());
        let cid = rel.with_extension("").display().to_string().replace('\\', "/");
        // frontmatter 字段命中
        let mut where_hit: Vec<&str> = Vec::new();
        if ftype.to_lowercase().contains(&kw_l) {
            where_hit.push("type");
        }
        if title.to_lowercase().contains(&kw_l) {
            where_hit.push("title");
        }
        if let Some(d) = fm_str(&fm, "description") {
            if d.to_lowercase().contains(&kw_l) {
                where_hit.push("description");
            }
        }
        if fm_tags(&fm).iter().any(|t| t.to_lowercase().contains(&kw_l)) {
            where_hit.push("tags");
        }
        if !where_hit.is_empty() {
            hits += 1;
            println!("{:<16} {cid} [{}]", ftype, where_hit.join(","));
            println!("{:<16}   {title}", "");
            continue;
        }
        // 正文行命中（输出行号 + 片段）
        let mut body_lines: Vec<(usize, String)> = Vec::new();
        for (i, line) in body.lines().enumerate() {
            if line.to_lowercase().contains(&kw_l) {
                let t = line.trim();
                if !t.is_empty() {
                    body_lines.push((i + 1, t.to_string()));
                }
            }
        }
        if !body_lines.is_empty() {
            hits += 1;
            println!("{:<16} {cid}", ftype);
            for (ln, text) in body_lines.iter().take(3) {
                let snippet = if text.len() > 90 { format!("{}…", &text[..90]) } else { text.clone() };
                println!("{:<16}   L{ln}: {snippet}", "");
            }
        }
    }
    println!("okf: {hits} concept(s) matched `{kw}` in library");
    Ok(())
}

// ─────────────────────────── 分发 ───────────────────────────

/// `yggd okf <libs|lib> <sub> ...`
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let head = args.get(3).map(|s| s.as_str()).unwrap_or("help");
    match head {
        "libs" => {
            let sub = args.get(4).map(|s| s.as_str()).unwrap_or("help");
            match sub {
                "new" => cmd_lib_new(args),
                "list" => cmd_lib_list(args),
                "show" => cmd_lib_show(args),
                "rm" => cmd_lib_rm(args),
                "validate" => cmd_lib_validate(args),
                "help" | "--help" | "-h" => {
                    println!("{}", USAGE);
                    Ok(())
                }
                other => anyhow::bail!("unknown libs subcommand `{other}`\n{USAGE}"),
            }
        }
        "help" | "--help" | "-h" => {
            println!("{}", USAGE);
            Ok(())
        }
        lib => {
            // 库内操作：yggd okf <lib> <add|rm|edit|list|show|index|log>
            let sub = args.get(4).map(|s| s.as_str()).unwrap_or("help");
            match sub {
                "add" => cmd_add(args),
                "rm" => cmd_rm(args),
                "restore" => cmd_restore(args),
                "edit" => cmd_edit(args),
                "list" => cmd_list(args),
                "show" => cmd_show(args),
                "index" => cmd_index(args),
                "log" => cmd_log(args),
                "search" => cmd_search(args),
                "help" | "--help" | "-h" => {
                    println!("{}", USAGE);
                    Ok(())
                }
                other => {
                    if valid_lib_name(lib) {
                        anyhow::bail!("unknown operation `{other}` for library `{lib}`\n{USAGE}");
                    } else {
                        anyhow::bail!("invalid library name `{lib}` or unknown subcommand\n{USAGE}");
                    }
                }
            }
        }
    }
}

const USAGE: &str = r#"yggd okf — OKF v0.2 knowledge libraries on the NSMT share dir
Layout: <NSMT_OKF_ROOT>/<library>/  = one OKF bundle per library (default root: <share>/okf)

Library management:
  yggd okf libs new <name> [--title X] [--description D]    Create a library (bundle)
  yggd okf libs list                                         List libraries
  yggd okf libs show <name>                                  Library details (concepts by type, log)
  yggd okf libs rm <name> --force                            Remove a library
  yggd okf libs validate <name> [--lint]                     Check OKF conformance (§11); --lint adds
                                                             status enum / ISO time / reserved-file /
                                                             link-resolution checks (okft-equivalent)

Concept CRUD inside a library:
  yggd okf <lib> add <rel-path> --type T [--title X] [--description D] [--resource URI] [--tags a,b] [--status draft|stable|deprecated]
  yggd okf <lib> rm <rel-path>                Move to .trash/ (recoverable)
  yggd okf <lib> restore <rel-path>           Restore from .trash/
  yggd okf <lib> edit <rel-path> [--type T] [--title X] [--description D] [--resource URI] [--tags a,b] [--status S] [--stale-after ISO8601]
  yggd okf <lib> list [--type T]
  yggd okf <lib> show <rel-path>
  yggd okf <lib> search <keyword>             Full-text search across frontmatter + body
  yggd okf <lib> index                       Refresh index.md per directory (§8)
  yggd okf <lib> log <message>               Append log.md entry (§9)

OKF v0.2 rules enforced: type required (§4.1); reserved filenames index.md/log.md
(§3.1); concept id = path minus .md (§2); generated.by actor process:nsmt (§7);
index.md carries no frontmatter, per-directory links are relative (§8); unknown
frontmatter keys preserved on edit (§4.1); removal records **Deprecation** in log.md (§9)."#;

// ─────────────────────────── 单元测试 ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_doc ----
    #[test]
    fn parse_doc_parses_frontmatter_and_body() {
        let doc = "---\ntype: Metric\ntitle: R2\n---\n\n# R2\n\nbody text\n";
        let (fm, body) = parse_doc(doc).expect("parse");
        assert_eq!(fm_str(&fm, "type").unwrap(), "Metric");
        assert_eq!(fm_str(&fm, "title").unwrap(), "R2");
        assert!(body.contains("# R2"));
        assert!(body.contains("body text"));
    }

    #[test]
    fn parse_doc_tolerates_bom() {
        let doc = "\u{feff}---\ntype: Reference\n---\n\n# X\n";
        let (fm, _) = parse_doc(doc).expect("parse with BOM");
        assert_eq!(fm_str(&fm, "type").unwrap(), "Reference");
    }

    #[test]
    fn parse_doc_handles_inline_generated() {
        let doc = "---\ntype: Metric\ngenerated: { by: process:nsmt, at: 2026-08-31T06:00:00Z }\n---\n\nbody\n";
        let (fm, _) = parse_doc(doc).expect("parse");
        let g = fm.get("generated").expect("generated present");
        assert_eq!(g.get("by").and_then(|v| v.as_str()).unwrap(), "process:nsmt");
        assert_eq!(g.get("at").and_then(|v| v.as_str()).unwrap(), "2026-08-31T06:00:00Z");
    }

    #[test]
    fn parse_doc_none_when_no_frontmatter() {
        assert!(parse_doc("# no frontmatter\n").is_none());
        assert!(parse_doc("").is_none());
        assert!(parse_doc("---\nnot: [valid: yaml\n---\n").is_none());
    }

    #[test]
    fn parse_doc_keeps_tags_sequence_and_inline() {
        let seq = "---\ntype: X\ntags:\n- a\n- b\n---\n\nbody\n";
        let (fm, _) = parse_doc(seq).expect("parse");
        assert_eq!(fm_tags(&fm), vec!["a".to_string(), "b".to_string()]);
        let inline = "---\ntype: X\ntags: [a, b]\n---\n\nbody\n";
        let (fm, _) = parse_doc(inline).expect("parse");
        assert_eq!(fm_tags(&fm), vec!["a".to_string(), "b".to_string()]);
        let csv = "---\ntype: X\ntags: a, b\n---\n\nbody\n";
        let (fm, _) = parse_doc(csv).expect("parse");
        assert_eq!(fm_tags(&fm), vec!["a".to_string(), "b".to_string()]);
    }

    // ---- civil_from_days / iso_now ----
    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn civil_from_days_known_dates() {
        // 2026-08-31 = 20696 days since epoch
        assert_eq!(civil_from_days(20696), (2026, 8, 31));
        assert_eq!(civil_from_days(20235), (2025, 5, 27));
        assert_eq!(civil_from_days(19723), (2024, 1, 1));
    }

    #[test]
    fn iso_now_format() {
        let s = iso_now();
        // YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[13..14], ":");
        assert_eq!(&s[16..17], ":");
        // year within reasonable range
        let year: u32 = s[..4].parse().unwrap();
        assert!((2024..=2030).contains(&year));
    }

    // ---- valid_lib_name ----
    #[test]
    fn lib_name_validation() {
        assert!(valid_lib_name("epdheat"));
        assert!(valid_lib_name("heating-analysis"));
        assert!(valid_lib_name("a.b_c-1"));
        assert!(!valid_lib_name(""));
        assert!(!valid_lib_name("Upper"));
        assert!(!valid_lib_name("has space"));
        assert!(!valid_lib_name("../../etc"));
        assert!(!valid_lib_name(&"x".repeat(64)));
    }

    // ---- reserved filenames ----
    #[test]
    fn reserved_names_exact() {
        assert_eq!(RESERVED, ["index.md", "log.md"]);
    }

    // ---- concept id semantics（§2：路径去 .md）----
    #[test]
    fn concept_id_is_path_minus_md() {
        let rel = PathBuf::from("tables/orders.md");
        let cid = rel.with_extension("").display().to_string().replace('\\', "/");
        assert_eq!(cid, "tables/orders");
    }

    // ---- load_concept round-trip ----
    #[test]
    fn load_concept_round_trip() {
        let dir = std::env::temp_dir().join("nsmt-okf-test-roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.md");
        std::fs::write(&p, "---\ntype: Metric\ntitle: R2\nstatus: stable\n---\n\n# R2\n").unwrap();
        let (fm, body) = load_concept(&p).expect("load");
        assert_eq!(fm_str(&fm, "status").unwrap(), "stable");
        assert!(body.contains("# R2"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
