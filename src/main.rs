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

    /// 启动命令后等待窗口出现的毫秒数（覆盖配置里的 launch_wait_ms）
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
    /// 隐藏（最小化）pad 的窗口
    Hide { pad: Option<String> },
    /// 列出配置中的 pad 和当前打开的窗口
    List,
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
        Command::List => {
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
            let kind = cfg.as_ref().map(|c| c.backend).unwrap_or_default();
            let viewport = cfg.as_ref().and_then(|c| c.viewport);
            let pads = match cfg {
                Some(c) => c.all_pads()?,
                None => Vec::new(),
            };
            let mut backend = open_backend(kind, viewport)?;
            ops::list(backend.as_mut(), &pads)
        }
        Command::Toggle { pad } => {
            let (cfg, pad) = load_pad(cli, pad.as_deref())?;
            let mut backend = open_backend(cfg.backend, cfg.viewport)?;
            ops::toggle(backend.as_mut(), &pad, wait(cli, &cfg))
        }
        Command::Show { pad } => {
            let (cfg, pad) = load_pad(cli, pad.as_deref())?;
            let mut backend = open_backend(cfg.backend, cfg.viewport)?;
            ops::show(backend.as_mut(), &pad, wait(cli, &cfg))
        }
        Command::Hide { pad } => {
            let (cfg, pad) = load_pad(cli, pad.as_deref())?;
            let mut backend = open_backend(cfg.backend, cfg.viewport)?;
            ops::hide(backend.as_mut(), &pad)
        }
    }
}

fn wait(cli: &Cli, cfg: &config::Config) -> Duration {
    Duration::from_millis(cli.wait.unwrap_or(cfg.launch_wait_ms))
}

fn load_pad(cli: &Cli, name: Option<&str>) -> Result<(config::Config, config::Pad)> {
    let path = config::config_path(cli.config.as_deref())?;
    let cfg = config::load(&path).with_context(|| {
        format!(
            "读取配置 {} 失败（示例见项目 README 或用 -c 指定路径）",
            path.display()
        )
    })?;
    let pad = cfg.resolve(name)?;
    Ok((cfg, pad))
}

fn open_backend(
    kind: BackendKind,
    viewport: Option<(f64, f64)>,
) -> Result<Box<dyn backend::Backend>> {
    match kind {
        BackendKind::Driftwm => {
            if !driftwm::DriftSession::available() {
                bail!("driftwm IPC 不可用（driftwm msg state 失败），无法使用 driftwm 后端");
            }
            Ok(Box::new(driftwm::DriftSession::new(viewport)?))
        }
        BackendKind::ForeignToplevel => open_toplevel(),
        BackendKind::Auto => {
            if driftwm::DriftSession::available() {
                Ok(Box::new(driftwm::DriftSession::new(viewport)?))
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
