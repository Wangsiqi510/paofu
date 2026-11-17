//! 把岛仔图标作为 RT_GROUP_ICON #1 编进 exe，供 LoadImageW(hInst, 1, ...) 取用。
//! 不引入额外依赖：直接调用 Windows SDK 的 rc.exe，把 .res 交给链接器。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn find_rc() -> Option<PathBuf> {
    let roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];
    let mut found: Vec<PathBuf> = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else { continue };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let rc = dir.join("x64").join("rc.exe");
            if rc.exists() {
                found.push(rc);
            }
        }
    }
    found.sort();
    found.pop()
}

/// 在 PATH 里找可执行文件，供没有 rc.exe 的交叉编译环境使用。
fn find_in_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths).map(|dir| dir.join(name)).find(|candidate| candidate.exists())
}

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/daozai.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("daozai.res");
    if let Some(rc) = find_rc() {
        let status = Command::new(&rc)
            .arg("/nologo")
            .arg("/fo")
            .arg(&out)
            .arg("app.rc")
            .status();
        if !status.map(|s| s.success()).unwrap_or(false) {
            println!("cargo:warning=rc.exe 编译 app.rc 失败");
            return;
        }
        println!("cargo:rustc-link-arg={}", out.display());
        return;
    }
    // 交叉编译（x86_64-pc-windows-gnu）没有 rc.exe：改用 mingw 的 windres，
    // 编成 COFF 目标文件交给 GNU 链接器，资源编号与自编 rc.exe 时一致。
    let Some(windres) = find_in_path("x86_64-w64-mingw32-windres").or_else(|| find_in_path("windres")) else {
        println!("cargo:warning=rc.exe 与 windres 都未找到，跳过图标资源");
        return;
    };
    let object = PathBuf::from(env::var("OUT_DIR").unwrap()).join("daozai-res.o");
    let status = Command::new(&windres).arg("app.rc").arg("-O").arg("coff").arg("-o").arg(&object).status();
    if !status.map(|s| s.success()).unwrap_or(false) {
        println!("cargo:warning=windres 编译 app.rc 失败");
        return;
    }
    println!("cargo:rustc-link-arg={}", object.display());
}
