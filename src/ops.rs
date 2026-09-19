//! pad 与窗口匹配后的具体动作。

use crate::backend::{Backend, Win};
use crate::config::Pad;
use anyhow::{bail, Context, Result};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

fn matching(wins: &[Win], pad: &Pad) -> Vec<Win> {
    wins.iter()
        .filter(|w| pad.matches(&w.app_id, &w.title))
        .cloned()
        .collect()
}

/// 全屏切换等操作会让窗口在 state 里短暂抖动；
/// 空结果先等一小段重查一次，避免误判为"无窗口"而重复 launch
fn matching_twice(backend: &mut dyn Backend, pad: &Pad) -> Result<Vec<Win>> {
    let wins = matching(&backend.snapshot()?, pad);
    if wins.is_empty() {
        std::thread::sleep(Duration::from_millis(250));
        return Ok(matching(&backend.snapshot()?, pad));
    }
    Ok(wins)
}

fn launch(pad: &Pad) -> Result<()> {
    let Some(cmd) = pad.spec.launch.as_deref() else {
        bail!(
            "没有匹配 '{}' 的窗口，且配置里也没有为它设置 launch 命令",
            pad.name
        );
    };
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("启动命令失败: {cmd}"))?;
    Ok(())
}

/// 启动 pad 的命令并等待匹配窗口出现（后端负责显示聚焦）
fn launch_and_wait(backend: &mut dyn Backend, pad: &Pad, wait: Duration) -> Result<String> {
    launch(pad)?;
    if backend.wait_for(pad, wait)? {
        Ok(format!("已启动并显示 '{}'", pad.name))
    } else {
        bail!(
            "已启动 '{}'，但 {}ms 内未出现匹配窗口",
            pad.name,
            wait.as_millis()
        );
    }
}

/// 窗口几何配置只有 driftwm 后端能落实，其他后端给出一次性提示
fn warn_unsupported_geometry(backend: &dyn Backend, pad: &Pad) {
    if pad.has_geometry() && backend.backend_name() != "driftwm" {
        eprintln!(
            "way-pad: pad '{}' 配置了窗口几何（width/height/edge/margin/fullscreen），\
             仅 driftwm 后端支持，当前后端 {} 将忽略",
            pad.name,
            backend.backend_name()
        );
    }
}

pub fn toggle(backend: &mut dyn Backend, pad: &Pad, launch_wait: Duration) -> Result<String> {
    warn_unsupported_geometry(backend, pad);
    let wins = matching_twice(backend, pad)?;
    if wins.is_empty() {
        return launch_and_wait(backend, pad, launch_wait);
    }
    if wins.iter().any(|w| !w.hidden) {
        for w in &wins {
            backend.hide(pad, w)?;
        }
        backend.sync()?;
        Ok(format!("已隐藏 '{}'（{} 个窗口）", pad.name, wins.len()))
    } else {
        for w in &wins {
            backend.reveal(pad, w, pad.spec.focus_on_show)?;
        }
        backend.sync()?;
        Ok(format!("已显示 '{}'（{} 个窗口）", pad.name, wins.len()))
    }
}

pub fn show(backend: &mut dyn Backend, pad: &Pad, launch_wait: Duration) -> Result<String> {
    warn_unsupported_geometry(backend, pad);
    let wins = matching_twice(backend, pad)?;
    if wins.is_empty() {
        return launch_and_wait(backend, pad, launch_wait);
    }
    for w in &wins {
        backend.reveal(pad, w, pad.spec.focus_on_show)?;
    }
    backend.sync()?;
    Ok(format!("已显示 '{}'（{} 个窗口）", pad.name, wins.len()))
}

pub fn hide(backend: &mut dyn Backend, pad: &Pad) -> Result<String> {
    let wins = matching(&backend.snapshot()?, pad);
    if wins.is_empty() {
        return Ok(format!("'{}' 没有匹配的窗口，无需隐藏", pad.name));
    }
    for w in &wins {
        backend.hide(pad, w)?;
    }
    backend.sync()?;
    Ok(format!("已隐藏 '{}'（{} 个窗口）", pad.name, wins.len()))
}

pub fn close(backend: &mut dyn Backend, pad: &Pad) -> Result<String> {
    let wins = matching_twice(backend, pad)?;
    if wins.is_empty() {
        return Ok(format!("'{}' 没有匹配的窗口", pad.name));
    }
    for w in &wins {
        backend.close(w)?;
    }
    Ok(format!("已关闭 '{}'（{} 个窗口）", pad.name, wins.len()))
}

pub fn list(backend: &mut dyn Backend, pads: &[Pad]) -> Result<String> {
    let wins = backend.snapshot()?;
    let mut out = String::new();

    out.push_str(&format!("backend: {}\n\n", backend.backend_name()));

    if pads.is_empty() {
        out.push_str("Pads: （配置中没有定义 pad）\n");
    } else {
        out.push_str("Pads:\n");
        for p in pads {
            out.push_str(&format!(
                "  {:<16} app_id='{}'{}  launch={}\n",
                p.name,
                p.spec.app_id,
                if p.spec.title.is_empty() {
                    String::new()
                } else {
                    format!(" title='{}'", p.spec.title)
                },
                p.spec.launch.as_deref().unwrap_or("-"),
            ));
        }
    }

    out.push_str("\nWindows:\n");
    if wins.is_empty() {
        out.push_str("  （没有窗口）\n");
    }
    for w in &wins {
        let mut flags = Vec::new();
        if w.hidden {
            flags.push("hidden");
        }
        if w.focused {
            flags.push("focused");
        }
        let matched = pads
            .iter()
            .filter(|p| p.matches(&w.app_id, &w.title))
            .map(|p| format!("pad:{}", p.name))
            .collect::<Vec<_>>()
            .join(" ");
        let matched = if matched.is_empty() {
            String::new()
        } else {
            format!(" {matched}")
        };
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}] ", flags.join(","))
        };
        out.push_str(&format!(
            "  {:<22} app_id='{}' title='{}'{}{}\n",
            backend.describe(w),
            w.app_id,
            w.title,
            flags,
            matched,
        ));
    }
    Ok(out)
}

// ---- list --json ----

#[derive(serde::Serialize)]
struct PadJson {
    name: String,
    app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    launch: Option<String>,
    matched_windows: usize,
}

#[derive(serde::Serialize)]
struct WinJson {
    key: String,
    app_id: String,
    title: String,
    hidden: bool,
    focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<(f64, f64)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_pad: Option<String>,
}

#[derive(serde::Serialize)]
struct ListJson {
    backend: String,
    pads: Vec<PadJson>,
    windows: Vec<WinJson>,
}

pub fn list_json(backend: &mut dyn Backend, pads: &[Pad]) -> Result<String> {
    let wins = backend.snapshot()?;
    let list = ListJson {
        backend: backend.backend_name().to_string(),
        pads: pads
            .iter()
            .map(|p| PadJson {
                name: p.name.clone(),
                app_id: p.spec.app_id.clone(),
                title: (!p.spec.title.is_empty()).then(|| p.spec.title.clone()),
                launch: p.spec.launch.clone(),
                matched_windows: wins
                    .iter()
                    .filter(|w| p.matches(&w.app_id, &w.title))
                    .count(),
            })
            .collect(),
        windows: wins
            .iter()
            .map(|w| WinJson {
                key: w.key.clone(),
                app_id: w.app_id.clone(),
                title: w.title.clone(),
                hidden: w.hidden,
                focused: w.focused,
                position: w.position,
                matched_pad: pads
                    .iter()
                    .find(|p| p.matches(&w.app_id, &w.title))
                    .map(|p| p.name.clone()),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&list).context("序列化 list 输出失败")
}

/// `doctor`：诊断配置、后端与各 pad 的匹配情况
pub fn doctor(backend: &mut dyn Backend, pads: &[Pad], config_path: &str) -> Result<String> {
    let wins = backend.snapshot()?;
    let mut out = String::new();

    out.push_str(&format!("配置文件: {config_path}\n"));
    out.push_str(&format!(
        "后端: {}\n窗口总数: {}\n\n",
        backend.backend_name(),
        wins.len()
    ));

    out.push_str("Pads:\n");
    for p in pads {
        let n = wins
            .iter()
            .filter(|w| p.matches(&w.app_id, &w.title))
            .count();
        let geo = if p.spec.fullscreen {
            "fullscreen".to_string()
        } else {
            let mut parts = Vec::new();
            if let Some(w) = &p.spec.width {
                parts.push(format!("width={w:?}"));
            }
            if let Some(h) = &p.spec.height {
                parts.push(format!("height={h:?}"));
            }
            if let Some(e) = p.spec.edge {
                parts.push(format!("edge={e:?}"));
            }
            if p.spec.margin != 0 {
                parts.push(format!("margin={}", p.spec.margin));
            }
            if parts.is_empty() {
                "无".to_string()
            } else {
                parts.join(" ")
            }
        };
        out.push_str(&format!(
            "  {:<14} 匹配窗口: {}  launch: {}  几何: {}\n",
            p.name,
            n,
            p.spec.launch.as_deref().unwrap_or("（未配置）"),
            geo,
        ));
        if n == 0 {
            match p.spec.launch.as_deref() {
                Some(cmd) => out.push_str(&format!(
                    "                 （无匹配窗口；按键时将启动: {cmd}）\n"
                )),
                None => {
                    out.push_str("                 （无匹配窗口；未配置 launch，按键时将报错）\n")
                }
            }
        }
    }

    let mut hidden = 0;
    let mut focused = 0;
    for w in &wins {
        if w.hidden {
            hidden += 1;
        }
        if w.focused {
            focused += 1;
        }
    }
    out.push_str(&format!("\n状态: {hidden} 个隐藏，{focused} 个聚焦\n"));
    Ok(out)
}
