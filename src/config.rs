//! 配置文件解析与 pad 定义。
//!
//! 默认位置 `~/.config/way-pad/config.toml`，可用 `-c` 或环境变量
//! `WAY_PAD_CONFIG` 覆盖。

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

const DEFAULT_LAUNCH_WAIT_MS: u64 = 1500;

/// 隐藏窗口的实现方式
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    /// 自动探测：是 driftwm 就用其 IPC，否则用 foreign-toplevel 协议
    #[default]
    Auto,
    /// 标准 zwlr_foreign_toplevel_management_v1（最小化/还原）
    ForeignToplevel,
    /// driftwm 专属 IPC（移出画布实现隐藏）
    Driftwm,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 隐藏窗口的后端
    #[serde(default)]
    pub backend: BackendKind,
    /// driftwm 后端判断"窗口是否在当前视野内"所用的视口尺寸（物理像素），
    /// 通常设为显示器分辨率
    #[serde(default)]
    pub viewport: Option<(f64, f64)>,
    /// `launch` 启动后等待匹配窗口出现的毫秒数
    #[serde(default = "default_launch_wait_ms")]
    pub launch_wait_ms: u64,
    /// pad 名字 -> pad 定义；保留书写顺序
    #[serde(default)]
    pub pads: indexmap::IndexMap<String, PadSpec>,
}

fn default_launch_wait_ms() -> u64 {
    DEFAULT_LAUNCH_WAIT_MS
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PadSpec {
    /// 匹配窗口 app_id 的正则表达式（必填）
    pub app_id: String,
    /// 匹配窗口标题的正则表达式；省略或留空表示不限定
    #[serde(default)]
    pub title: String,
    /// 没有匹配窗口时要启动的命令（经 `sh -c` 执行）
    #[serde(default)]
    pub launch: Option<String>,
    /// 显示窗口时是否同时聚焦（默认 true）
    #[serde(default = "default_true")]
    pub focus_on_show: bool,
    /// 显示时窗口宽度（屏幕像素）；省略则保持窗口当前宽度
    #[serde(default)]
    pub width: Option<u32>,
    /// 显示时窗口高度（屏幕像素）；省略则保持窗口当前高度
    #[serde(default)]
    pub height: Option<u32>,
    /// 窗口距视野边缘的边距（屏幕像素），默认 0
    #[serde(default)]
    pub margin: u32,
    /// 窗口停靠边：top/bottom/left/right/center（默认 top）
    #[serde(default)]
    pub edge: Option<Edge>,
    /// 显示时进入全屏；true 时忽略 width/height/edge/margin（默认 false）
    #[serde(default)]
    pub fullscreen: bool,
}

impl Default for PadSpec {
    fn default() -> Self {
        PadSpec {
            app_id: String::new(),
            title: String::new(),
            launch: None,
            focus_on_show: true,
            width: None,
            height: None,
            margin: 0,
            edge: None,
            fullscreen: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// 窗口停靠边（相对当前视野）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
    Center,
}

/// 编译好正则、可直接使用的 pad
#[derive(Debug)]
pub struct Pad {
    pub name: String,
    pub spec: PadSpec,
    app_id: Regex,
    title: Option<Regex>,
}

impl Pad {
    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        self.app_id.is_match(app_id)
            && self.title.as_ref().is_none_or(|r| r.is_match(title))
    }

    /// 是否配置了窗口几何（width/height/edge/margin/fullscreen 任一）
    pub fn has_geometry(&self) -> bool {
        let s = &self.spec;
        s.fullscreen || s.width.is_some() || s.height.is_some() || s.margin != 0
            || s.edge.is_some()
    }
}

pub fn config_path(explicit: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(p) = std::env::var("WAY_PAD_CONFIG") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var("HOME").context("无法定位配置文件：HOME 未设置")?)
            .join(".config"),
    };
    Ok(base.join("way-pad").join("config.toml"))
}

pub fn load(path: &std::path::Path) -> Result<Config> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("无法读取配置文件 {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&raw).with_context(|| format!("解析 {} 失败", path.display()))?;
    cfg.validate()?;
    Ok(cfg)
}

impl Config {
    fn validate(&self) -> Result<()> {
        if self.pads.is_empty() {
            bail!("配置中没有定义任何 pad（需要 [pads.<名字>] 段）");
        }
        for (name, spec) in &self.pads {
            if spec.app_id.is_empty() {
                bail!("pad '{name}' 缺少 app_id");
            }
            if let Err(e) = Regex::new(&spec.app_id) {
                bail!("pad '{name}' 的 app_id 不是合法正则表达式: {e}");
            }
            if !spec.title.is_empty() {
                if let Err(e) = Regex::new(&spec.title) {
                    bail!("pad '{name}' 的 title 不是合法正则表达式: {e}");
                }
            }
        }
        Ok(())
    }

    fn pad(&self, name: &str) -> Result<Pad> {
        let names = || {
            self.pads
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        let spec = self
            .pads
            .get(name)
            .with_context(|| format!("配置中没有名为 '{name}' 的 pad（可用: {}）", names()))?;
        Ok(Pad {
            name: name.to_string(),
            spec: spec.clone(),
            app_id: Regex::new(&spec.app_id).expect("validate 已校验过正则"),
            title: if spec.title.is_empty() {
                None
            } else {
                Some(Regex::new(&spec.title).expect("validate 已校验过正则"))
            },
        })
    }

    /// 解析命令行给出的 pad 名；未给出且配置里只有一个 pad 时直接使用它
    pub fn resolve(&self, name: Option<&str>) -> Result<Pad> {
        match name {
            Some(n) => self.pad(n),
            None => {
                if self.pads.len() == 1 {
                    self.pad(self.pads.keys().next().unwrap())
                } else {
                    bail!(
                        "未指定 pad 名（可用: {}）",
                        self.pads.keys().cloned().collect::<Vec<_>>().join(", ")
                    );
                }
            }
        }
    }

    pub fn all_pads(&self) -> Result<Vec<Pad>> {
        self.pads.keys().map(|n| self.pad(n)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        let cfg: Config = toml::from_str(s).expect("解析失败");
        cfg.validate().expect("校验失败");
        cfg
    }

    #[test]
    fn parses_pads_and_defaults() {
        let cfg = parse(
            r#"
[pads.term]
app_id = '^foot$'
launch = 'foot'

[pads.music]
app_id = 'mpv'
title = 'mpv —.*'
focus_on_show = false
"#,
        );
        assert_eq!(cfg.launch_wait_ms, DEFAULT_LAUNCH_WAIT_MS);
        let term = cfg.resolve(Some("term")).unwrap();
        assert!(term.spec.focus_on_show);
        assert!(term.matches("foot", "任意标题"));
        assert!(!term.matches("footclient", "x"));
        let music = cfg.resolve(Some("music")).unwrap();
        assert!(!music.spec.focus_on_show);
        assert!(music.matches("mpv", "mpv — 视频"));
        assert!(!music.matches("mpv", "其他"));
    }

    #[test]
    fn resolves_single_pad_without_name() {
        let cfg = parse("[pads.only]\napp_id = 'x'\n");
        assert_eq!(cfg.resolve(None).unwrap().name, "only");
    }

    #[test]
    fn rejects_bad_regex_and_unknown_fields() {
        let cfg: Config = toml::from_str("[pads.bad]\napp_id = '('\n").unwrap();
        assert!(cfg.validate().is_err());
        let cfg: Result<Config, _> = toml::from_str("[pads.a]\napp_id='x'\nwrong_field=1\n");
        assert!(cfg.is_err());
    }

    #[test]
    fn rejects_config_without_pads() {
        let cfg: Config = toml::from_str("launch_wait_ms = 10\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn empty_title_pattern_matches_anything() {
        let cfg = parse("[pads.a]\napp_id = 'abc'\ntitle = ''\n");
        let p = cfg.resolve(Some("a")).unwrap();
        assert!(p.title.is_none());
        assert!(p.matches("abc", ""));
    }

    #[test]
    fn map_preserves_order() {
        let cfg = parse("[pads.z]\napp_id='z'\n\n[pads.a]\napp_id='a'\n");
        let keys: Vec<_> = cfg.pads.keys().cloned().collect();
        assert_eq!(keys, vec!["z", "a"]);
    }

    #[test]
    fn pad_not_found_lists_names() {
        let cfg = parse("[pads.a]\napp_id='a'\n");
        let err = format!("{}", cfg.resolve(Some("b")).unwrap_err());
        assert!(err.contains("'b'") && err.contains("a"));
    }

    #[test]
    fn default_pad_spec() {
        let d = PadSpec::default();
        assert!(d.launch.is_none());
        assert!(d.focus_on_show);
        assert_eq!(indexmap::IndexMap::<String, PadSpec>::new().len(), 0);
    }
}
