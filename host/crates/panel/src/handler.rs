//! "Open in default app": the checked handler, with the roadmap's Windows
//! readings. The checks run in order, and each refuses before anything opens:
//!
//! 1. private roots (and the lexical path gate before that);
//! 2. executables: a PE `MZ` header, `#!`, ELF or Mach-O magic;
//! 3. launch-type extensions (`.exe .bat .cmd .ps1 …`), on files and folders;
//! 4. packages: Windows has no bundles, so a plain folder opens in Explorer;
//! 5. the handler: looked up from the file association (a query, never a launch), refused when there
//!    is none, when it is on the deny list (shells, script hosts, installers, settings, Explorer as a
//!    *file* handler…), when it is outside a trusted install location, or when it lives inside the
//!    item's own git checkout.
//!
//! Opening re-runs every check, then asks the shell to open the file with its default verb.

use crate::private::PrivateRoots;
use crate::viewer::{admit, ReadError};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const LAUNCH_EXTENSIONS: &[&str] = &[
    "exe",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "lnk",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "scr",
    "com",
    "pif",
    "reg",
    "hta",
    "cpl",
    "msc",
    "url",
    "website",
    "application",
    "appref-ms",
    "jar",
    "inf",
    "scf",
    "settingcontent-ms",
    "library-ms",
    "search-ms",
    "diagcab",
    "msp",
    "gadget",
    "ws",
    "xbap",
];

/// Executable names never accepted as a document handler, compared case-insensitively.
pub const DENIED_HANDLERS: &[&str] = &[
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "powershell_ise.exe",
    "wscript.exe",
    "cscript.exe",
    "mshta.exe",
    "rundll32.exe",
    "regedit.exe",
    "reg.exe",
    "regsvr32.exe",
    "msiexec.exe",
    "explorer.exe",
    "control.exe",
    "systemsettings.exe",
    "wt.exe",
    "windowsterminal.exe",
    "conhost.exe",
    "openconsole.exe",
    "bash.exe",
    "wsl.exe",
    "sh.exe",
    "mintty.exe",
    "git-bash.exe",
    "java.exe",
    "javaw.exe",
    "python.exe",
    "pythonw.exe",
    "py.exe",
    "pyw.exe",
    "node.exe",
    "perl.exe",
    "ruby.exe",
    "php.exe",
    "appinstaller.exe",
    "installer.exe",
    "scrnsave.scr",
    "schtasks.exe",
    "taskmgr.exe",
    "mmc.exe",
    "hh.exe",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Handler {
    pub name: String,
    pub app_path: String,
}

/// Which check refused, in the order they run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "step", content = "reason")]
pub enum Refusal {
    Path(String),
    Private(String),
    Executable(String),
    LaunchType(String),
    NoHandler(String),
    DeniedHandler(String),
    UntrustedHandler(String),
}

impl Refusal {
    pub fn reason(&self) -> &str {
        match self {
            Self::Path(r)
            | Self::Private(r)
            | Self::Executable(r)
            | Self::LaunchType(r)
            | Self::NoHandler(r)
            | Self::DeniedHandler(r)
            | Self::UntrustedHandler(r) => r,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// A plain folder: Explorer shows it.
    Folder(PathBuf),
    File(PathBuf, Handler),
}

/// True for PE (`MZ`), `#!`, ELF and Mach-O (including the fat magic Java class files share).
pub fn is_executable_header(head: &[u8]) -> bool {
    if head.starts_with(b"MZ") || head.starts_with(b"#!") || head.starts_with(b"\x7fELF") {
        return true;
    }
    if head.len() >= 4 {
        let m = [head[0], head[1], head[2], head[3]];
        return matches!(
            m,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
        );
    }
    false
}

pub fn is_launch_extension(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // Windows ignores trailing dots and spaces: `a.exe.` launches as `a.exe`.
    let name = name.trim_end_matches(['.', ' ']);
    LAUNCH_EXTENSIONS
        .iter()
        .any(|ext| name.ends_with(&format!(".{ext}")))
}

pub fn is_denied_handler(app_path: &str) -> bool {
    let name = app_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(app_path)
        .to_ascii_lowercase();
    DENIED_HANDLERS.contains(&name.as_str())
}

fn under(path: &str, root: &str) -> bool {
    let p = path.replace('/', "\\").to_lowercase();
    let r = root
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    !r.is_empty() && p.starts_with(&format!("{r}\\"))
}

/// The install locations a handler may live in (system and per-user program folders).
pub fn trusted_roots() -> Vec<String> {
    let mut out = Vec::new();
    for var in [
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "SystemRoot",
    ] {
        if let Some(v) = std::env::var_os(var) {
            out.push(v.to_string_lossy().into_owned());
        }
    }
    if let Some(v) = std::env::var_os("LOCALAPPDATA") {
        let base = PathBuf::from(v);
        out.push(base.join("Programs").to_string_lossy().into_owned());
        out.push(
            base.join("Microsoft")
                .join("WindowsApps")
                .to_string_lossy()
                .into_owned(),
        );
    }
    out
}

pub fn is_trusted_location(app_path: &str, trusted: &[String]) -> bool {
    trusted.iter().any(|r| under(app_path, r))
}

/// The top of the git checkout containing `path`, if any (a `.git` folder or file beside an ancestor).
pub fn checkout_root(path: &Path) -> Option<PathBuf> {
    let mut cur = path.parent();
    while let Some(dir) = cur {
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// Runs the checks in order. `lookup` answers "which app opens this extension?" (a query only).
pub fn check(
    raw: &str,
    roots: &PrivateRoots,
    trusted: &[String],
    lookup: &dyn Fn(&Path) -> Option<Handler>,
) -> Result<Decision, Refusal> {
    // 1. The path gate, then private roots.
    let path = admit(raw, roots).map_err(|e| match e {
        ReadError::Refused(m) if m == crate::viewer::PRIVATE_MESSAGE => Refusal::Private(m),
        other => Refusal::Path(other.message()),
    })?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| Refusal::Path(e.to_string()))?;
    if helm_platform::fs::is_link_like(&meta) {
        return Err(Refusal::Path("Links and shortcuts are not opened.".into()));
    }
    // 2. Executables, by content.
    if meta.is_file() {
        let mut head = [0u8; 4];
        let n = helm_platform::fs::open_regular_no_follow(&path)
            .and_then(|mut f| f.read(&mut head))
            .map_err(|e| Refusal::Path(e.to_string()))?;
        if is_executable_header(&head[..n]) {
            return Err(Refusal::Executable(
                "This file is a program, so Helm does not open it.".into(),
            ));
        }
    }
    // 3. Launch-type extensions, on files and folders.
    if is_launch_extension(&path) {
        return Err(Refusal::LaunchType(
            "Helm does not open files of this type.".into(),
        ));
    }
    // 4. Packages: a plain folder opens in Explorer.
    if meta.is_dir() {
        return Ok(Decision::Folder(path));
    }
    if !meta.is_file() {
        return Err(Refusal::Path(
            "Only regular files and folders are opened.".into(),
        ));
    }
    // 5. The handler.
    let handler = lookup(&path).ok_or_else(|| Refusal::NoHandler("No default app".into()))?;
    if is_denied_handler(&handler.app_path) {
        return Err(Refusal::DeniedHandler(format!(
            "{} is not used to open documents.",
            handler.name
        )));
    }
    if !is_trusted_location(&handler.app_path, trusted) {
        return Err(Refusal::UntrustedHandler(format!(
            "{} is not installed in a trusted location.",
            handler.name
        )));
    }
    if let Some(top) = checkout_root(&path) {
        if under(&handler.app_path, &top.to_string_lossy()) {
            return Err(Refusal::UntrustedHandler(
                "The default app lives inside this file's own repository.".into(),
            ));
        }
    }
    Ok(Decision::File(path, handler))
}

/// The OS association for the path's extension: the app's executable and display name.
#[cfg(windows)]
pub fn os_lookup(path: &Path) -> Option<Handler> {
    use windows_sys::Win32::UI::Shell::{
        AssocQueryStringW, ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_NOTRUNCATE, ASSOCSTR_EXECUTABLE,
        ASSOCSTR_FRIENDLYAPPNAME,
    };
    let ext = format!(".{}", path.extension()?.to_string_lossy());
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let ext_w = wide(&ext);
    let verb = wide("open");
    let query = |what| -> Option<String> {
        let mut buf = vec![0u16; 2048];
        let mut len = buf.len() as u32;
        // SAFETY: all pointers are valid NUL-terminated strings or a buffer of `len` u16s.
        let hr = unsafe {
            AssocQueryStringW(
                ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE,
                what,
                ext_w.as_ptr(),
                verb.as_ptr(),
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        if hr != 0 || len == 0 {
            return None;
        }
        let s = String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]);
        (!s.trim().is_empty()).then_some(s)
    };
    let app_path = query(ASSOCSTR_EXECUTABLE)?;
    let name = query(ASSOCSTR_FRIENDLYAPPNAME).unwrap_or_else(|| {
        Path::new(&app_path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Some(Handler { name, app_path })
}

#[cfg(not(windows))]
pub fn os_lookup(_path: &Path) -> Option<Handler> {
    None
}

/// Opens a checked decision: Explorer for a folder, the shell's default verb for a file.
#[cfg(windows)]
pub fn open(decision: &Decision) -> std::io::Result<()> {
    match decision {
        Decision::Folder(p) => explorer(&[p.as_os_str().to_os_string()]),
        Decision::File(p, _) => {
            use windows_sys::Win32::UI::Shell::ShellExecuteW;
            const SW_SHOWNORMAL: i32 = 1;
            let wide = |s: &std::ffi::OsStr| {
                use std::os::windows::ffi::OsStrExt;
                s.encode_wide()
                    .chain(std::iter::once(0))
                    .collect::<Vec<u16>>()
            };
            let file = wide(p.as_os_str());
            let verb = wide(std::ffi::OsStr::new("open"));
            // SAFETY: the strings are NUL-terminated and live for the call; null means "none".
            let r = unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    file.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                )
            };
            if (r as isize) <= 32 {
                return Err(std::io::Error::other(
                    "The default app could not be started.",
                ));
            }
            Ok(())
        }
    }
}

#[cfg(not(windows))]
pub fn open(_decision: &Decision) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "Opening in the default app is Windows-only.",
    ))
}

/// Explorer with the item selected (`/select,`). The path has passed the gate and private roots.
#[cfg(windows)]
pub fn reveal(path: &Path) -> std::io::Result<()> {
    let mut arg = std::ffi::OsString::from("/select,");
    arg.push(path.as_os_str());
    explorer(&[arg])
}

#[cfg(not(windows))]
pub fn reveal(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("Reveal is Windows-only."))
}

#[cfg(windows)]
fn explorer(args: &[std::ffi::OsString]) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let exe = PathBuf::from(windir).join("explorer.exe");
    let mut cmd = std::process::Command::new(exe);
    for a in args {
        // Explorer parses its own command line: quote the path part ourselves.
        let s = a.to_string_lossy();
        match s.strip_prefix("/select,") {
            Some(rest) => cmd.raw_arg(format!("/select,\"{rest}\"")),
            None => cmd.raw_arg(format!("\"{s}\"")),
        };
    }
    cmd.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn trusted() -> Vec<String> {
        vec![r"C:\Program Files".into(), r"C:\Windows".into()]
    }

    fn app(path: &str) -> Handler {
        Handler {
            name: "App".into(),
            app_path: path.into(),
        }
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    /// One temp file per step, each refusing at its own step before the handler is ever asked.
    #[test]
    fn each_step_refuses_in_order_before_anything_opens() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Helm");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();
        let roots = PrivateRoots::new(std::slice::from_ref(&root));
        let asked = Cell::new(0);
        let lookup = |_: &Path| {
            asked.set(asked.get() + 1);
            Some(app(r"C:\Program Files\Editor\editor.exe"))
        };

        // 1. Private root.
        assert!(matches!(
            check(&s(&root.join("notes.txt")), &roots, &trusted(), &lookup),
            Err(Refusal::Private(_))
        ));
        // 2. Executable content behind a harmless extension.
        for (name, bytes) in [
            // A program behind a launch-type extension must fail at step 2, not 3: the order is
            // part of the contract.
            ("run.ps1", b"MZ\x90\x00".as_slice()),
            ("pe.txt", b"MZ\x90\x00".as_slice()),
            ("script.txt", b"#!/bin/sh".as_slice()),
            ("elf.dat", b"\x7fELF".as_slice()),
            ("macho.dat", b"\xcf\xfa\xed\xfe".as_slice()),
            ("fat.class", b"\xca\xfe\xba\xbe".as_slice()),
        ] {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            assert!(
                matches!(
                    check(&s(&p), &roots, &trusted(), &lookup),
                    Err(Refusal::Executable(_))
                ),
                "{name}"
            );
        }
        // 3. Launch-type extension, with harmless content; also on a folder and with a trailing dot.
        for name in [
            "run.ps1",
            "a.lnk",
            "setup.msi",
            "x.hta",
            "y.CPL",
            "page.url",
        ] {
            let p = dir.path().join(name);
            std::fs::write(&p, "plain text").unwrap();
            assert!(
                matches!(
                    check(&s(&p), &roots, &trusted(), &lookup),
                    Err(Refusal::LaunchType(_))
                ),
                "{name}"
            );
        }
        let folder_exe = dir.path().join("Tool.exe");
        std::fs::create_dir(&folder_exe).unwrap();
        assert!(matches!(
            check(&s(&folder_exe), &roots, &trusted(), &lookup),
            Err(Refusal::LaunchType(_))
        ));
        assert_eq!(asked.get(), 0, "no step before 5 may ask for the handler");

        // 4. A plain folder goes to Explorer, still without asking for a handler.
        let plain = dir.path().join("docs");
        std::fs::create_dir(&plain).unwrap();
        assert!(matches!(
            check(&s(&plain), &roots, &trusted(), &lookup),
            Ok(Decision::Folder(_))
        ));
        assert_eq!(asked.get(), 0);

        // 5. The handler: none, denied, untrusted, then accepted.
        let doc = dir.path().join("report.pdf");
        std::fs::write(&doc, "%PDF-1.7").unwrap();
        assert!(matches!(
            check(&s(&doc), &roots, &trusted(), &|_| None),
            Err(Refusal::NoHandler(_))
        ));
        assert!(matches!(
            check(&s(&doc), &roots, &trusted(), &|_| Some(app(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\PowerShell.exe"
            ))),
            Err(Refusal::DeniedHandler(_))
        ));
        assert!(matches!(
            check(&s(&doc), &roots, &trusted(), &|_| Some(app(
                r"C:\Windows\explorer.exe"
            ))),
            Err(Refusal::DeniedHandler(_))
        ));
        assert!(matches!(
            check(&s(&doc), &roots, &trusted(), &|_| Some(app(
                r"D:\Downloads\viewer.exe"
            ))),
            Err(Refusal::UntrustedHandler(_))
        ));
        assert!(matches!(
            check(&s(&doc), &roots, &trusted(), &|_| Some(app(
                r"C:\Program FilesX\viewer.exe"
            ))),
            Err(Refusal::UntrustedHandler(_))
        ));
        assert_eq!(
            check(&s(&doc), &roots, &trusted(), &lookup),
            Ok(Decision::File(
                doc.clone(),
                app(r"C:\Program Files\Editor\editor.exe")
            ))
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_anywhere_along_the_path_is_refused_for_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir_all(target.join("inner")).unwrap();
        std::fs::write(target.join("a.pdf"), "%PDF").unwrap();
        let link = dir.path().join("link");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(
            made,
            "mklink /J needs no privilege; the test must not skip itself"
        );
        for p in [link.clone(), link.join("inner"), link.join("a.pdf")] {
            let r = check(&s(&p), &PrivateRoots::default(), &trusted(), &|_| {
                panic!("asked")
            });
            assert!(matches!(r, Err(Refusal::Path(_))), "{p:?}: {r:?}");
        }
    }

    #[test]
    fn a_handler_inside_the_files_own_checkout_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::create_dir(dir.path().join("tools")).unwrap();
        let doc = dir.path().join("a.pdf");
        std::fs::write(&doc, "%PDF").unwrap();
        let inside = s(&dir.path().join("tools").join("viewer.exe"));
        let trusted = vec![s(dir.path())];
        let r = check(&s(&doc), &PrivateRoots::default(), &trusted, &|_| {
            Some(app(&inside))
        });
        assert!(matches!(r, Err(Refusal::UntrustedHandler(_))), "{r:?}");
    }

    #[test]
    fn the_gate_refuses_shares_before_any_check() {
        #[cfg(windows)]
        for p in [
            r"\\host\share\a.pdf",
            "//host/share/a.pdf",
            r"\\?\C:\a.pdf",
            "C:a.pdf",
        ] {
            assert!(
                matches!(
                    check(p, &PrivateRoots::default(), &trusted(), &|_| panic!(
                        "asked"
                    )),
                    Err(Refusal::Path(_))
                ),
                "{p}"
            );
        }
    }

    #[test]
    fn deny_list_and_launch_extensions_are_case_insensitive() {
        assert!(is_denied_handler(r"C:\Windows\System32\CMD.EXE"));
        assert!(!is_denied_handler(
            r"C:\Program Files\Notepad++\notepad++.exe"
        ));
        assert!(is_launch_extension(Path::new("A.BAT")));
        assert!(is_launch_extension(Path::new("a.exe. ")));
        assert!(!is_launch_extension(Path::new("a.json")));
    }
}
