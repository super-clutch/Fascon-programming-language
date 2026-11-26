//! 0.1 系のエンジン（同梱の fascon-core = Fascon 0.1.0）への委譲 / delegation to the 0.1 engine (bundled fascon-core = Fascon 0.1.0)

use std::path::PathBuf;
use std::process::Command;

/// fascon-core の場所: 環境変数 FASCON_CORE → この実行ファイルと同じフォルダ
/// where fascon-core is: the FASCON_CORE environment variable, then the folder of this executable
pub fn core_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FASCON_CORE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) { "fascon-core.exe" } else { "fascon-core" };
    let p = dir.join(name);
    if p.is_file() {
        make_executable(&p);
        return Some(p);
    }
    None
}

#[cfg(unix)]
fn make_executable(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(p) {
        let mode = m.permissions().mode();
        if mode & 0o111 == 0 {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode | 0o755));
        }
    }
}

#[cfg(not(unix))]
fn make_executable(_p: &std::path::Path) {}

/// 引数をそのまま渡して実行し、終了コードを返す / run with the arguments unchanged and return the exit code
pub fn run(args: &[String]) -> Option<i32> {
    let core = core_path()?;
    let st = Command::new(core).args(args).status().ok()?;
    Some(st.code().unwrap_or(1))
}

/// 標準出力を取り込んで実行する / run and capture stdout
pub fn capture(args: &[&str]) -> Option<(i32, String)> {
    let core = core_path()?;
    let out = Command::new(core).args(args).output().ok()?;
    Some((out.status.code().unwrap_or(1), String::from_utf8_lossy(&out.stdout).to_string()))
}

pub fn version() -> Option<String> {
    capture(&["version"]).map(|(_, s)| s.trim().to_string())
}
