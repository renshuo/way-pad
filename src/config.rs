//! 配置文件解析与 pad 定义。
//!
//! 默认位置 `~/.config/way-pad/config.toml`，可用 `-c` 或环境变量
//! `WAY_PAD_CONFIG` 覆盖。

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

const DEFAULT_LAUNCH_WAIT_MS: u64 = 1500;

/// 隐藏窗口的后端
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

/// 隐藏方式（仅 driftwm 后端支持 opacity）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HideMode {
    /// 移到画布极远的藏匿点（默认）
    #[default]
    Move,
    /// 窗口原地全透明。注意：透明窗口通常仍会拦截鼠标点击
    Opacity,
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

/// 窗口尺寸：像素或视口百分比（如 "90%"）
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Size {
    Px(u32),
    Percent(String),
}

impl Size {
    /// 换算成屏幕像素。`viewport_side` 为对应的视口宽/高
    pub fn resolve_px(&self, viewport_side: f64) -> Result<f64> {
        match self {
            Size::Px(v) => Ok(*v as f64),
            Size::Percent(s) => {
                let p: f64 = s
                    .strip_suffix('%')
                    .context(format!("百分比尺寸格式错误: {s:?}（示例 \"90%\"）"))?
                    .trim()
                    .parse()
                    .context(format!("百分比尺寸格式错误: {s:?}（示例 \"90%\"）"))?;
                if !(0.0..=100.0).contains(&p) {
                    bail!("百分比尺寸超出 0-100%: {s:?}");
                }
                Ok(p / 100.0 * viewport_side)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 隐藏窗口的后端
    #[serde(default)]
    pub backend: BackendKind,
    /// 隐藏方式（仅 driftwm 后端支持 opacity）
    #[serde(default)]
    pub hide_mode: HideMode,
    /// driftwm 后端判断"窗口是否在当前视野内"所用的视口尺寸（物理像素），
    /// 通常设为显示器分辨率
    #[serde(default)]
    pub viewport: Option<(f64, f64)>,
    /// `launch` 启动后等待匹配窗口出现的默认毫秒数（可被 pad 级覆盖）
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
    /// 该 pad 启动命令后等待窗口出现的毫秒数；省略则用顶层值
    #[serde(default)]
    pub launch_wait_ms: Option<u64>,
    /// 显示时窗口宽度（屏幕像素，或视口百分比如 "90%"）；省略则保持当前宽度
    #[serde(default)]
    pub width: Option<Size>,
    /// 显示时窗口高度（同上）
    #[serde(default)]
    pub height: Option<Size>,
    /// 窗口距视野边缘的边距（屏幕像素），默认 0
    #[serde(default)]
    pub margin: u32,
    /// 窗口停靠边：top/bottom/left/right/center（默认 top）
    #[serde(default)]
    pub edge: Option<Edge>,
    /// 显示时占满当前视野；true 时忽略 width/height/edge/margin（默认 false）
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
            launch_wait_ms: None,
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
        self.app_id.is_match(app_id) && self.title.as_ref().is_none_or(|r| r.is_match(title))
    }

    /// 是否配置了窗口几何（width/height/edge/margin/fullscreen 任一）
    pub fn has_geometry(&self) -> bool {
        let s = &self.spec;
        s.fullscreen || s.width.is_some() || s.height.is_some() || s.margin != 0 || s.edge.is_some()
    }

    /// 该 pad 生效的 launch 等待时间（pad 级覆盖顶层）
    pub fn launch_wait_ms(&self, global: u64) -> u64 {
        self.spec.launch_wait_ms.unwrap_or(global)
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
            if let Some(Size::Percent(s)) = &spec.width {
                Size::Percent(s.clone()).resolve_px(1920.0)?;
            }
            if let Some(Size::Percent(s)) = &spec.height {
                Size::Percent(s.clone()).resolve_px(1080.0)?;
            }
        }
        Ok(())
    }

    fn pad(&self, name: &str) -> Result<Pad> {
        let names = || self.pads.keys().cloned().collect::<Vec<_>>().join(", ");
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
        assert_eq!(cfg.hide_mode, HideMode::Move);
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

    #[test]
    fn parses_hide_mode_and_sizes() {
        let cfg = parse(
            r#"
hide_mode = "opacity"

[pads.a]
app_id = 'x'
width = 1600
height = "90%"
"#,
        );
        assert_eq!(cfg.hide_mode, HideMode::Opacity);
        let p = cfg.resolve(Some("a")).unwrap();
        assert!(matches!(p.spec.width, Some(Size::Px(1600))));
        assert_eq!(
            p.spec.height.as_ref().unwrap().resolve_px(1080.0).unwrap(),
            972.0
        );
    }

    #[test]
    fn rejects_bad_percent() {
        let cfg: Config = toml::from_str("[pads.a]\napp_id='x'\nheight = \"120%\"\n").unwrap();
        assert!(cfg.validate().is_err());
        let cfg: Config = toml::from_str("[pads.a]\napp_id='x'\nheight = \"abc%\"\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn pad_level_wait_overrides_global() {
        let cfg = parse(
            r#"
launch_wait_ms = 1500

[pads.slow]
app_id = 'x'
launch_wait_ms = 9000

[pads.fast]
app_id = 'y'
"#,
        );
        let slow = cfg.resolve(Some("slow")).unwrap();
        let fast = cfg.resolve(Some("fast")).unwrap();
        assert_eq!(slow.launch_wait_ms(cfg.launch_wait_ms), 9000);
        assert_eq!(fast.launch_wait_ms(cfg.launch_wait_ms), 1500);
    }

    #[test]
    fn has_geometry_variants() {
        let cfg = parse(
            r#"
[pads.a]
app_id = 'x'
margin = 5

[pads.b]
app_id = 'y'
fullscreen = true

[pads.c]
app_id = 'z'
"#,
        );
        assert!(cfg.resolve(Some("a")).unwrap().has_geometry());
        assert!(cfg.resolve(Some("b")).unwrap().has_geometry());
        assert!(!cfg.resolve(Some("c")).unwrap().has_geometry());
    }
}
