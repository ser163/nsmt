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

/// 收集 bundle 内 concept 文件（跳过保留文件），返回相对路径列表
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

/// 收集知识库（根下含 index.md 或任何 .md 的目录）
fn collect_libs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = e.file_name().to_string_lossy().to_string();
                if valid_lib_name(&name) && has_md(&p) {
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

/// 生成 index.md（§8：按目录分组；库根 index.md 带 okf_version frontmatter）
fn generate_index(root: &Path, root_frontmatter: bool) -> anyhow::Result<()> {
    let files = collect_md_files(root);
    let mut groups: std::collections::BTreeMap<String, Vec<PathBuf>> = Default::default();
    for rel in &files {
        let dir = rel.parent().map(|p| p.display().to_string()).unwrap_or_else(|| ".".into());
        groups.entry(dir).or_default().push(rel.clone());
    }
    let mut out = String::new();
    if root_frontmatter {
        out.push_str(&format!("---\nokf_version: {OKF_VERSION}\n---\n\n"));
    }
    out.push_str("# Knowledge Bundle\n\n");
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
        out.push_str("_No concepts yet. Run `yggd okf <lib> add <path> --type <Type>`._\n");
    }
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
    // 根 index.md 带 okf_version 声明（§8/§12）
    let mut idx = format!("---\nokf_version: {OKF_VERSION}\n");
    if let Some(t) = opts.get("title") {
        if !t.is_empty() {
            idx.push_str(&format!("title: {t}\n"));
        }
    }
    if let Some(d) = opts.get("description") {
        if !d.is_empty() {
            idx.push_str(&format!("description: {d}\n"));
        }
    }
    idx.push_str("---\n\n# Knowledge Bundle\n\n_Empty library. Run `yggd okf <lib> add <path> --type <Type>`._\n");
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

/// `yggd okf libs validate <name>`
fn cmd_lib_validate(args: &[String]) -> anyhow::Result<()> {
    let (root, opts) = parse_args(args, 5);
    let name = need(&opts, "path", "usage: yggd okf libs validate <name>")?;
    let dir = lib_dir(&root, &name);
    let (total, errors) = validate_bundle(&dir)?;
    println!("okf: validated library `{name}` — {total} document(s), {errors} error(s)");
    if errors > 0 {
        std::process::exit(1);
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
    let status = opts.get("status").cloned().unwrap_or_else(|| "draft".into());

    let mut fm = format!("type: {ftype}\n");
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

    let doc = format!("---\n{fm}---\n\n# {title}\n\n");
    std::fs::write(&target, doc)?;
    append_log(&dir, &format!("**Creation**: Established [{}](/{})", title, rel.replace('\\', "/")))?;
    println!("okf: concept created {rel} (type={ftype})");
    Ok(())
}

/// `yggd okf <lib> rm <rel-path>`
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
    std::fs::remove_file(&target)?;
    append_log(&dir, &format!("**Deprecation**: Removed [{}](/{})", rel, rel.replace('\\', "/")))?;
    println!("okf: concept removed {rel}");
    Ok(())
}

/// `yggd okf <lib> edit <rel-path> [--type T] [--title X] [--description D] [--tags a,b] [--status S]`
/// 保留未知 frontmatter 字段（§4.1），更新 generated.at（§5.2）
fn cmd_edit(args: &[String]) -> anyhow::Result<()> {
    let (dir, opts) = resolve_lib(args)?;
    let rel = need(&opts, "path", "usage: yggd okf <lib> edit <rel-path> [--title X] ...")?;
    let target = dir.join(&rel);
    let raw = std::fs::read_to_string(&target).map_err(|_| anyhow::anyhow!("concept not found: {}", target.display()))?;
    let (mut fm, body) = parse_doc(&raw).ok_or_else(|| anyhow::anyhow!("not a valid OKF concept: {}", target.display()))?;

    if let Some(t) = opts.get("type") {
        if !t.is_empty() {
            set_fm(&mut fm, "type", serde_yaml::Value::String(t.clone()));
        }
    }
    if let Some(t) = opts.get("title") {
        if !t.is_empty() {
            set_fm(&mut fm, "title", serde_yaml::Value::String(t.clone()));
        }
    }
    if let Some(d) = opts.get("description") {
        if !d.is_empty() {
            set_fm(&mut fm, "description", serde_yaml::Value::String(d.clone()));
        }
    }
    if let Some(t) = opts.get("tags") {
        if !t.is_empty() {
            let list: Vec<String> = t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            set_fm(&mut fm, "tags", serde_yaml::Value::Sequence(list.into_iter().map(serde_yaml::Value::String).collect()));
        }
    }
    if let Some(s) = opts.get("status") {
        if !s.is_empty() {
            set_fm(&mut fm, "status", serde_yaml::Value::String(s.clone()));
        }
    }
    // 内容变更 → 更新 generated.at（保留 generated.by）
    let gen = serde_yaml::Value::Mapping({
        let mut m = serde_yaml::Mapping::new();
        if let Some(g) = fm.get("generated") {
            if let Some(by) = g.get("by") {
                m.insert(serde_yaml::Value::String("by".into()), by.clone());
            }
        }
        m.insert(serde_yaml::Value::String("at".into()), serde_yaml::Value::String(iso_now()));
        m
    });
    set_fm(&mut fm, "generated", gen);

    let fm_yaml = serde_yaml::to_string(&fm).map_err(|e| anyhow::anyhow!("yaml serialize: {e}"))?;
    std::fs::write(&target, format!("---\n{fm_yaml}---\n\n{body}"))?;
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
        let title = fm_str(&fm, "title").unwrap_or_else(|| rel.display().to_string());
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
    generate_index(&dir, true)?;
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
                "edit" => cmd_edit(args),
                "list" => cmd_list(args),
                "show" => cmd_show(args),
                "index" => cmd_index(args),
                "log" => cmd_log(args),
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
  yggd okf libs validate <name>                              Check OKF conformance (§11)

Concept CRUD inside a library:
  yggd okf <lib> add <rel-path> --type T [--title X] [--description D] [--tags a,b] [--status draft|stable|deprecated]
  yggd okf <lib> rm <rel-path>
  yggd okf <lib> edit <rel-path> [--type T] [--title X] [--description D] [--tags a,b] [--status S]
  yggd okf <lib> list [--type T]
  yggd okf <lib> show <rel-path>
  yggd okf <lib> index                       Refresh index.md (§8)
  yggd okf <lib> log <message>               Append log.md entry (§9)

OKF v0.2 rules enforced: type required (§4.1); reserved filenames index.md/log.md
(§3.1); concept id = path minus .md (§2); generated.by actor process:nsmt (§7);
root index.md carries okf_version (§12); unknown frontmatter keys preserved on
edit (§4.1); removal records **Deprecation** in log.md (§9)."#;
