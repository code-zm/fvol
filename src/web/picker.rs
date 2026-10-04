//! The desktop's own "open file" dialog, for the page's "Open file…" button.
//!
//! A browser never tells a page the full path of a file chosen in its picker, and fastvol needs
//! the path (it maps the image in place). The server runs on the analyst's machine, so it can
//! show the desktop's native dialog itself (kdialog on KDE, zenity elsewhere) and return the path.
//! Offered only when the server listens on loopback and has a display: a remote browser would
//! never see a dialog that opens on the server's screen.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// A dialog tool found on PATH.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool {
    Kdialog,
    Zenity,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Kdialog => "kdialog",
            Tool::Zenity => "zenity",
        }
    }
}

fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
}

/// The tool to use, if this process can show a dialog at all.
pub fn tool() -> Option<Tool> {
    let has_display = ["DISPLAY", "WAYLAND_DISPLAY"].iter().any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()));
    if !has_display {
        return None;
    }
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_ascii_uppercase().contains("KDE"));
    let order = if kde { [Tool::Kdialog, Tool::Zenity] } else { [Tool::Zenity, Tool::Kdialog] };
    order.into_iter().find(|t| on_path(t.name()))
}

pub enum Picked {
    Path(PathBuf),
    Cancelled,
}

static OPEN: AtomicBool = AtomicBool::new(false);

/// Show the dialog, starting in `dir`, and wait for the user. Only one dialog at a time.
pub fn pick(tool: Tool, dir: &Path) -> Result<Picked, String> {
    if OPEN.swap(true, Ordering::AcqRel) {
        return Err("a file dialog is already open on the desktop".into());
    }
    let _reset = Reset;
    let title = "Open a memory image — fastvol";
    let mut cmd = Command::new(tool.name());
    match tool {
        Tool::Kdialog => cmd.arg("--title").arg(title).arg("--getopenfilename").arg(dir),
        // a trailing slash makes zenity start inside the folder instead of selecting it
        Tool::Zenity => cmd.arg("--file-selection").arg(format!("--title={title}")).arg(format!("--filename={}/", dir.display())),
    };
    let out = cmd.output().map_err(|e| format!("cannot run {}: {e}", tool.name()))?;
    match out.status.code() {
        Some(0) => {
            let s = String::from_utf8_lossy(&out.stdout);
            let p = s.trim_end_matches(['\n', '\r']);
            if p.is_empty() { Ok(Picked::Cancelled) } else { Ok(Picked::Path(PathBuf::from(p))) }
        }
        Some(1) => Ok(Picked::Cancelled),
        _ => Err(format!("{} failed: {}", tool.name(), String::from_utf8_lossy(&out.stderr).trim())),
    }
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        OPEN.store(false, Ordering::Release);
    }
}
