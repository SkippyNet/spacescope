//! Operating-system integration: drives, opening files, the trash.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const FILE_MANAGER: &str = if cfg!(windows) {
    "Explorer"
} else {
    "file manager"
};
pub const TRASH: &str = if cfg!(windows) {
    "Recycle Bin"
} else {
    "Trash"
};

#[derive(Clone, Debug)]
pub struct Drive {
    pub mount: PathBuf,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub removable: bool,
}

/// Mounted volumes worth offering. May block on unreachable network drives,
/// so call it off the UI thread.
#[cfg(windows)]
pub fn drives() -> Vec<Drive> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    const REMOVABLE: u32 = 2;
    const FIXED: u32 = 3;
    const REMOTE: u32 = 4;
    const RAMDISK: u32 = 6;
    let text = |buf: &[u16]| {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end]).trim().to_string()
    };
    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        if ![REMOVABLE, FIXED, REMOTE, RAMDISK].contains(&kind) {
            continue;
        }
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
        if ok == 0 || total == 0 {
            continue;
        }
        let mut label = [0u16; 261];
        let mut fs = [0u16; 64];
        unsafe {
            GetVolumeInformationW(
                wide.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            );
        }
        let mut label = text(&label);
        if label.is_empty() {
            label = match kind {
                REMOVABLE => "Removable Disk",
                REMOTE => "Network Drive",
                _ => "Local Disk",
            }
            .to_string();
        }
        out.push(Drive {
            mount: PathBuf::from(root),
            label,
            fs: text(&fs),
            total,
            free: avail,
            removable: kind == REMOVABLE,
        });
    }
    out
}

/// Mounted volumes worth offering.
#[cfg(not(windows))]
pub fn drives() -> Vec<Drive> {
    use std::os::unix::ffi::OsStrExt;
    const PSEUDO: &[&str] = &[
        "squashfs",
        "tmpfs",
        "devtmpfs",
        "overlay",
        "efivarfs",
        "proc",
        "sysfs",
        "cgroup",
        "cgroup2",
        "ramfs",
        "autofs",
        "devpts",
        "nsfs",
        "securityfs",
        "debugfs",
        "tracefs",
        "pstore",
        "bpf",
        "mqueue",
        "hugetlbfs",
        "configfs",
        "fusectl",
        "binfmt_misc",
        "rpc_pipefs",
        "fuse.portal",
        "fuse.gvfsd-fuse",
        "fuse.snapfuse",
        "iso9660",
    ];
    const SKIP: &[&str] = &[
        "/snap",
        "/boot",
        "/run",
        "/sys",
        "/proc",
        "/dev",
        "/var/lib/docker",
        "/var/snap",
        "/init",
        "/mnt/wslg",
        "/usr/lib/wsl",
        "/tmp/.X11-unix",
    ];
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return Vec::new();
    };
    // /proc/mounts escapes space, tab and backslash as octal.
    let unescape = |s: &str| {
        s.replace(r"\040", " ")
            .replace(r"\011", "\t")
            .replace(r"\134", r"\")
    };
    let mut out: Vec<(String, Drive)> = Vec::new();
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let (Some(dev), Some(mount), Some(fs)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let mount = unescape(mount);
        if PSEUDO.contains(&fs)
            || SKIP
                .iter()
                .any(|p| mount == *p || mount.starts_with(&format!("{p}/")))
        {
            continue;
        }
        let path = PathBuf::from(&mount);
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            continue;
        };
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            continue;
        }
        let unit = st.f_frsize as u64;
        let total = st.f_blocks as u64 * unit;
        if total == 0 {
            continue;
        }
        let drive = Drive {
            mount: path,
            label: String::new(),
            fs: fs.to_string(),
            total,
            free: st.f_bavail as u64 * unit,
            removable: mount.starts_with("/media/") || mount.starts_with("/run/media/"),
        };
        // The same device mounted twice (bind mounts, subvolumes): keep the
        // shortest path.
        match out
            .iter_mut()
            .find(|(d, _)| d == dev && dev.starts_with('/'))
        {
            Some((_, existing)) => {
                if drive.mount.as_os_str().len() < existing.mount.as_os_str().len() {
                    *existing = drive;
                }
            }
            None => out.push((dev.to_string(), drive)),
        }
    }
    let mut out: Vec<Drive> = out.into_iter().map(|(_, d)| d).collect();
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    };
    norm(a) == norm(b)
}

/// `(total, free)` if `path` is exactly the root of a mounted volume.
pub fn volume_of(path: &Path, drives: &[Drive]) -> Option<(u64, u64)> {
    drives
        .iter()
        .find(|d| same_path(&d.mount, path))
        .map(|d| (d.total, d.free))
}

pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

pub fn open_path(path: &Path) -> Result<(), String> {
    open::that_detached(path).map_err(|e| format!("Could not open {}: {e}", path.display()))
}

/// Shows the item selected in the system file manager.
pub fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Could not start Explorer: {e}"))
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::process::Stdio;
        let mut uri = String::from("file://");
        for &b in path.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                uri.push(b as char);
            } else {
                uri.push_str(&format!("%{b:02X}"));
            }
        }
        let shown = Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{uri}"),
                "string:",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if shown {
            Ok(())
        } else {
            open_path(path.parent().unwrap_or(path))
        }
    }
}

pub fn trash(path: &Path) -> Result<(), String> {
    trash::delete(path)
        .map_err(|e| format!("Could not move {} to the {TRASH}: {e}", path.display()))
}

/// True when running with administrator rights (always false off Windows,
/// where the fast path this gates does not exist).
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let (mut elevated, mut len) = (0u32, 0u32);
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevated as *mut u32 as *mut _,
                4,
                &mut len,
            );
            CloseHandle(token);
            ok != 0 && elevated != 0
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Starts a new, elevated copy of the app (UAC prompt), optionally scanning
/// `path` straight away. The caller should then close this instance.
pub fn relaunch_elevated(path: Option<&Path>) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::path::MAIN_SEPARATOR;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(Some(0)).collect() };
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        // Quote the path. A trailing backslash (as in `C:\`) would escape
        // the closing quote, so it is doubled.
        let params = match path {
            Some(p) => {
                let mut s = p.display().to_string();
                if s.ends_with(MAIN_SEPARATOR) {
                    s.push(MAIN_SEPARATOR);
                }
                format!("\"{s}\"")
            }
            None => String::new(),
        };
        let (verb, exe, params) = (
            wide("runas".as_ref()),
            wide(exe.as_os_str()),
            wide(params.as_ref()),
        );
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                exe.as_ptr(),
                params.as_ptr(),
                std::ptr::null(),
                1, // SW_SHOWNORMAL
            )
        };
        if result as usize > 32 {
            Ok(())
        } else {
            Err("Administrator rights were not granted.".to_string())
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Not available on this system.".to_string())
    }
}
