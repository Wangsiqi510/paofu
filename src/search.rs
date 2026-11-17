//! 岛内搜索：应用、常用目录里的文件、Windows 联系人。
//!
//! 索引在第一次搜索时建立并常驻内存；如果本机装了 Everything 且能找到 `es.exe`，
//! 文件结果会优先走 Everything（只读、不复制、不写注册表），否则用轻量自建索引。
//! 日程与联系人由调用方补充（见 `App::page_search`）。

use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    App,
    File,
    Folder,
    Contact,
    Calendar,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub kind: Kind,
    pub title: String,
    pub detail: String,
    pub target: Target,
}

#[derive(Clone, Debug)]
pub enum Target {
    Path(PathBuf),
    Text(String),
    Calendar,
}

impl Hit {
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            Kind::App => "应用",
            Kind::File => "文件",
            Kind::Folder => "文件夹",
            Kind::Contact => "联系人",
            Kind::Calendar => "日程",
        }
    }
}

struct Index {
    apps: Vec<(String, PathBuf)>,
    files: Vec<(String, PathBuf)>,
    contacts: Vec<String>,
}

static INDEX: OnceLock<Index> = OnceLock::new();

fn home() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into()))
}

fn walk(root: &std::path::Path, depth: usize, max: usize, want: &dyn Fn(&std::path::Path) -> bool, out: &mut Vec<PathBuf>) {
    if depth == 0 || out.len() >= max { return; }
    let Ok(read) = std::fs::read_dir(root) else { return; };
    for entry in read.flatten() {
        if out.len() >= max { break; }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue; };
        if meta.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "Windows") { continue; }
            walk(&path, depth - 1, max, want, out);
        } else if meta.is_file() && want(&path) {
            out.push(path);
        }
    }
}

fn build() -> Index {
    let mut apps = Vec::new();
    let mut starts = Vec::new();
    if let Ok(program_data) = std::env::var("PROGRAMDATA") {
        starts.push(PathBuf::from(program_data).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    if let Ok(app_data) = std::env::var("APPDATA") {
        starts.push(PathBuf::from(app_data).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    for start in &starts {
        let mut paths = Vec::new();
        walk(start, 3, 600, &|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")), &mut paths);
        for p in paths {
            if let Some(stem) = p.file_stem() {
                let label = stem.to_string_lossy().to_string();
                if !labels_is_noise(&label) { apps.push((label, p)); }
            }
        }
    }
    apps.sort_by(|a, b| a.0.cmp(&b.0));
    apps.dedup_by(|a, b| a.0 == b.0);

    let mut files = Vec::new();
    let h = home();
    for dir in ["Desktop", "Downloads", "Documents"] {
        let mut paths = Vec::new();
        walk(&h.join(dir), 3, 2500, &|_| true, &mut paths);
        for p in paths {
            if let Some(name) = p.file_name() {
                files.push((name.to_string_lossy().to_string(), p));
            }
        }
    }

    let mut contacts = Vec::new();
    let mut paths = Vec::new();
    walk(&h.join("Contacts"), 2, 500, &|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("contact")), &mut paths);
    for p in paths {
        if let Some(stem) = p.file_stem() { contacts.push(stem.to_string_lossy().to_string()); }
    }
    contacts.sort();
    contacts.dedup();

    Index { apps, files, contacts }
}

fn labels_is_noise(label: &str) -> bool {
    ["卸载", "Uninstall", "Readme", "帮助", "Help", "更新", "Update"].iter().any(|n| label.contains(n))
}

pub fn index() -> &'static Index {
    INDEX.get_or_init(build)
}

/// Everything 的 `es.exe` 路径（存在才返回）。
pub fn everything_es() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("EVERYTHING_ES") {
        let p = PathBuf::from(path);
        if p.exists() { return Some(p); }
    }
    for guess in [
        r"C:\Program Files\Everything\es.exe",
        r"C:\Program Files (x86)\Everything\es.exe",
        r"D:\Everything\es.exe",
        r"D:\Program Files\Everything\es.exe",
        r"D:\Program Files (x86)\Everything\es.exe",
    ] {
        let p = PathBuf::from(guess);
        if p.exists() { return Some(p); }
    }
    None
}

fn everything_files(query: &str, limit: usize) -> Vec<Hit> {
    let Some(es) = everything_es() else { return Vec::new(); };
    let mut cmd = std::process::Command::new(es);
    cmd.arg("-n").arg(limit.to_string()).arg(query);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let Ok(out) = cmd.output() else { return Vec::new(); };
    if !out.status.success() { return Vec::new(); }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(limit)
        .map(|line| {
            let path = PathBuf::from(line);
            let title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| line.to_string());
            let detail = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
            Hit { kind: if path.is_dir() { Kind::Folder } else { Kind::File }, title, detail, target: Target::Path(path) }
        })
        .collect()
}

fn score(haystack: &str, needle: &str) -> Option<u32> {
    let hay = haystack.to_lowercase();
    let need = needle.to_lowercase();
    if need.is_empty() { return None; }
    if hay.starts_with(&need) { Some(0) }
    else if hay.contains(&need) { Some(1) }
    else { None }
}

pub fn search(query: &str, limit: usize) -> Vec<Hit> {
    let q = query.trim();
    if q.is_empty() { return Vec::new(); }
    let idx = index();
    let mut hits: Vec<(u32, Hit)> = Vec::new();
    {
        let mut push = |rank: u32, hit: Hit| {
            if hits.len() < limit * 4 { hits.push((rank, hit)); }
        };

        for (label, path) in &idx.apps {
            if let Some(rank) = score(label, q) {
                push(rank, Hit { kind: Kind::App, title: label.clone(), detail: path.display().to_string(), target: Target::Path(path.clone()) });
            }
        }
        for (name, path) in &idx.files {
            if let Some(rank) = score(name, q) {
                push(rank + 2, Hit { kind: Kind::File, title: name.clone(), detail: path.parent().map(|p| p.display().to_string()).unwrap_or_default(), target: Target::Path(path.clone()) });
            }
        }
        for name in &idx.contacts {
            if let Some(rank) = score(name, q) {
                push(rank + 1, Hit { kind: Kind::Contact, title: name.clone(), detail: "Windows 联系人".into(), target: Target::Text(name.clone()) });
            }
        }
    }

    // 自建索引不够时再用 Everything 补文件结果（只读查询，最多 8 条）。
    if everything_es().is_some() && hits.iter().filter(|(_, h)| matches!(h.kind, Kind::File | Kind::Folder)).count() < 3 {
        for hit in everything_files(q, limit.min(6)) {
            hits.push((4, hit));
        }
    }

    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.title.cmp(&b.1.title)));
    hits.dedup_by(|a, b| a.1.title == b.1.title && a.1.detail == b.1.detail);
    hits.into_iter().take(limit).map(|(_, h)| h).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_query_has_no_results() {
        assert!(search("  ", 8).is_empty());
    }
    #[test]
    fn scoring_prefers_prefix() {
        assert!(score("微信", "微").unwrap() < score("企业微信", "微").unwrap());
        assert!(score("微信", "信").is_some());
        assert!(score("微信", "钉").is_none());
    }
}
