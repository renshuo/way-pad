//! way-pad —— Wayland 下的 tdrop 式窗口开关。
//!
//! 按配置中的 pad（app_id/title 正则匹配 + 可选启动命令）对指定窗口
//! 做显示/隐藏/聚焦切换，用法与 tdrop 类似：把 `way-pad toggle <pad>`
//! 绑到合成器快捷键上即可。

mod backend;
mod config;
mod driftwm;
mod ops;
mod toplevel;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use config::BackendKind;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "way-pad",
    version,
    about = "tdrop 式的窗口显示/隐藏开关（基于 zwlr_foreign_toplevel_management_v1）",
    long_about = "way-pad 按 ~/.config/way-pad/config.toml 里定义的 pad 匹配窗口，\n\
                  对匹配窗口做显示/隐藏切换；没有窗口时可用 launch 配置自动拉起，\n\
                  类似 X11 下的 tdrop。适用于 driftwm 等 wlroots/smithay 系合成器。"
)]
struct Cli {
    /// 配置文件路径
    #[arg(short, long, value_name = "FILE", env = "WAY_PAD_CONFIG")]
    config: Option<PathBuf>,

    /// 启动命令后等待窗口出现的毫秒数（覆盖配置值）
    #[arg(short = 'w', long, value_name = "MS")]
    wait: Option<u64>,

    /// 只输出错误
    #[arg(short, long)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 切换 pad：有可见窗口则隐藏，否则显示并聚焦；没有窗口则按 launch 启动
    Toggle { pad: Option<String> },
    /// 显示并聚焦 pad 的窗口（没有窗口则按 launch 启动）
    Show { pad: Option<String> },
    /// 隐藏 pad 的窗口
    Hide { pad: Option<String> },
    /// 关闭 pad 的窗口
    Close { pad: Option<String> },
    /// 列出配置中的 pad 和当前打开的窗口
    List {
        /// 以 JSON 输出
        #[arg(long)]
        json: bool,
    },
    /// 诊断配置、后端与各 pad 的匹配情况
    Doctor,
}

/// pad 级互斥锁：防止快速连按/按键重复导致两个 way-pad 同时操作同一 pad。
/// 持有文件本身即为锁，字段无需读取
struct PadLock(#[allow(dead_code)] std::fs::File);

fn acquire_pad_lock(name: &str) -> Result<PadLock> {
    let dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp"),
    }
    .join("way-pad");
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建锁目录 {}", dir.display()))?;
    let path = dir.join(format!("{name}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("无法打开锁文件 {}", path.display()))?;
    // flock 由内核管理，进程退出（包括崩溃）自动释放
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        bail!("'{name}' 已有另一个 way-pad 正在处理（按键重复？），本次已忽略");
    }
    Ok(PadLock(file))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(msg) => {
            if !msg.is_empty() && !cli.quiet {
                println!("way-pad: {msg}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("way-pad: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<String> {
    match &cli.command {
        Command::List { json } => {
            let path = config::config_path(cli.config.as_deref())?;
            let cfg = match config::load(&path) {
                Ok(c) => Some(c),
                Err(e) => {
                    if !cli.quiet {
                        eprintln!("way-pad: 读取配置失败，只列出窗口: {e:#}");
                    }
                    None
                }
            };
            let (pads, kind, viewport, hide_mode) = match &cfg {
                Some(c) => (c.all_pads()?, c.backend, c.viewport, c.hide_mode),
                None => (Vec::new(), BackendKind::default(), None, Default::default()),
            };
            let mut backend = open_backend(kind, viewport, hide_mode)?;
            if *json {
                // JSON 供脚本消费，直接输出、不加 way-pad 前缀、不受 -q 影响
                println!("{}", ops::list_json(backend.as_mut(), &pads)?);
                return Ok(String::new());
            }
            ops::list(backend.as_mut(), &pads)
        }
        Command::Doctor => {
            let path = config::config_path(cli.config.as_deref())?;
            let cfg = config::load(&path)?;
            let pads = cfg.all_pads()?;
            let mut backend = open_backend(cfg.backend, cfg.viewport, cfg.hide_mode)?;
            ops::doctor(backend.as_mut(), &pads, &path.display().to_string())
        }
        Command::Toggle { pad } => {
            let (cfg, pad, _lock) = load_pad(cli, pad.as_deref())?;
            let mut backend = open_backend(cfg.backend, cfg.viewport, cfg.hide_mode)?;
            ops::toggle(backend.as_mut(), &pad, wait(cli, &cfg, &pad))
        }
        Command::Show { pad } => {
            let (cfg, pad, _lock) = load_pad(cli, pad.as_deref())?;
            let mut backend = open_backend(cfg.backend, cfg.viewport, cfg.hide_mode)?;
            ops::show(backend.as_mut(), &pad, wait(cli, &cfg, &pad))
        }
        Command::Hide { pad } => {
            let (cfg, pad, _lock) = load_pad(cli, pad.as_deref())?;
            let mut backend = open_backend(cfg.backend, cfg.viewport, cfg.hide_mode)?;
            ops::hide(backend.as_mut(), &pad)
        }
        Command::Close { pad } => {
            let (cfg, pad, _lock) = load_pad(cli, pad.as_deref())?;
            let mut backend = open_backend(cfg.backend, cfg.viewport, cfg.hide_mode)?;
            ops::close(backend.as_mut(), &pad)
        }
    }
}

/// 毫秒数优先级：命令行 -w > pad 级配置 > 顶层配置
fn wait(cli: &Cli, cfg: &config::Config, pad: &config::Pad) -> Duration {
    Duration::from_millis(
        cli.wait
            .unwrap_or_else(|| pad.launch_wait_ms(cfg.launch_wait_ms)),
    )
}

/// 加载配置并解析 pad，同时获取该 pad 的互斥锁
fn load_pad(cli: &Cli, name: Option<&str>) -> Result<(config::Config, config::Pad, PadLock)> {
    let path = config::config_path(cli.config.as_deref())?;
    let cfg = config::load(&path).with_context(|| {
        format!(
            "读取配置 {} 失败（示例见项目 README 或用 -c 指定路径）",
            path.display()
        )
    })?;
    let pad = cfg.resolve(name)?;
    let lock = acquire_pad_lock(&pad.name)?;
    Ok((cfg, pad, lock))
}

fn open_backend(
    kind: BackendKind,
    viewport: Option<(f64, f64)>,
    hide_mode: config::HideMode,
) -> Result<Box<dyn backend::Backend>> {
    match kind {
        BackendKind::Driftwm => {
            if !driftwm::DriftSession::available() {
                bail!("driftwm IPC 不可用（driftwm msg state 失败），无法使用 driftwm 后端");
            }
            Ok(Box::new(driftwm::DriftSession::new(viewport, hide_mode)?))
        }
        BackendKind::ForeignToplevel => {
            if hide_mode != config::HideMode::default() {
                eprintln!("way-pad: hide_mode = \"opacity\" 仅 driftwm 后端支持，将使用 move 方式");
            }
            open_toplevel()
        }
        BackendKind::Auto => {
            if driftwm::DriftSession::available() {
                Ok(Box::new(driftwm::DriftSession::new(viewport, hide_mode)?))
            } else {
                open_toplevel()
            }
        }
    }
}

fn open_toplevel() -> Result<Box<dyn backend::Backend>> {
    let session = toplevel::Session::connect()?;
    if !session.supported() {
        bail!(
            "合成器不支持 zwlr_foreign_toplevel_management_v1，way-pad 无法显示/隐藏窗口；\
             请确认运行于 driftwm 或其他支持该协议的合成器"
        );
    }
    Ok(Box::new(session))
}
