//! driftwm 专属后端：通过 `driftwm msg` IPC 控制窗口。
//!
//! driftwm 是无限画布合成器，没有最小化概念（foreign-toplevel 的
//! set_minimized 是 no-op）。隐藏语义实现为"把窗口移到画布极远的藏匿点"，
//! 显示时移回隐藏前记录的位置（或当前视野中心）并聚焦。

use crate::backend::{Backend, Win};
use crate::config::{Edge, Pad};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 藏匿点：无限画布上一个实际使用中不可能出现的坐标。
/// 要足够远——driftwm 的 window_placement=auto 会吸附"相邻集群"，
/// 藏匿窗口太近会把新窗口吸到屏幕外（实测 1e6 处曾被吸到 22 万）。
const HIDE_SPOT: (f64, f64) = (100_000_000.0, 100_000_000.0);
/// 判定窗口是否已在藏匿点附近的容差
const HIDE_SPOT_TOLERANCE: f64 = 5000.0;
/// launch 后轮询窗口出现的间隔
const POLL_INTERVAL: Duration = Duration::from_millis(120);
/// 新窗口出现后等待 driftwm 完成 auto placement 的时间
const PLACEMENT_SETTLE: Duration = Duration::from_millis(250);

pub struct DriftSession {
    positions_path: PathBuf,
    /// 视口尺寸（物理像素），用于判断窗口是否在当前视野内
    viewport: (f64, f64),
}

/// 无限画布上 tdrop 的核心体验：窗口恢复位置若不在当前视野内，
/// 就把它带到视野中心，保证按快捷键后总能看到它
fn in_viewport(pos: (f64, f64), cam: (f64, f64), zoom: f64, viewport: (f64, f64)) -> bool {
    let zoom = zoom.max(0.1);
    let (hw, hh) = (viewport.0 * 0.55 / zoom, viewport.1 * 0.55 / zoom);
    (pos.0 - cam.0).abs() <= hw && (pos.1 - cam.1).abs() <= hh
}

/// `driftwm msg state --json` 的回复：{"Ok": {"State": {...}}}
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(rename = "Ok")]
    ok: Option<OkPayload>,
    #[serde(rename = "Err")]
    err: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OkPayload {
    #[serde(rename = "State")]
    state: Option<DriftState>,
}

#[derive(Debug, Deserialize)]
struct DriftState {
    #[serde(default)]
    camera: Vec<f64>,
    #[serde(default)]
    zoom: f64,
    #[serde(default)]
    windows: Vec<DriftWindow>,
}

#[derive(Debug, Clone, Deserialize)]
struct DriftWindow {
    id: i64,
    #[serde(default)]
    app_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    position: Vec<f64>,
    #[serde(default)]
    size: Vec<f64>,
    #[serde(default)]
    is_focused: bool,
}

/// 按停靠边计算窗口中心（画布坐标）。margin/w/h 均为屏幕像素，
/// 换算成画布坐标时除以 zoom；Y-up 坐标系，y 越大越靠屏幕上方。
fn edge_position(
    edge: Edge,
    margin: f64,
    w: f64,
    h: f64,
    cam: (f64, f64),
    zoom: f64,
    viewport: (f64, f64),
) -> (f64, f64) {
    let z = zoom.max(0.1);
    let m = margin / z;
    let w = w / z;
    let h = h / z;
    let hw = viewport.0 / 2.0 / z;
    let hh = viewport.1 / 2.0 / z;
    match edge {
        Edge::Top => (cam.0, cam.1 + hh - m - h / 2.0),
        Edge::Bottom => (cam.0, cam.1 - hh + m + h / 2.0),
        Edge::Left => (cam.0 - hw + m + w / 2.0, cam.1),
        Edge::Right => (cam.0 + hw - m - w / 2.0, cam.1),
        Edge::Center => cam,
    }
}

/// pad -> 窗口 id -> 隐藏前的画布位置
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Positions(BTreeMap<String, BTreeMap<String, (f64, f64)>>);

fn xy(v: &[f64]) -> (f64, f64) {
    (v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0))
}

fn in_hide_spot((x, y): (f64, f64)) -> bool {
    (x - HIDE_SPOT.0).abs() <= HIDE_SPOT_TOLERANCE
        && (y - HIDE_SPOT.1).abs() <= HIDE_SPOT_TOLERANCE
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.cache")))?;
    Some(PathBuf::from(base).join("way-pad"))
}

impl DriftSession {
    /// 探测当前会话是否是 driftwm（以及 IPC 是否可用）
    pub fn available() -> bool {
        Command::new("driftwm")
            .args(["msg", "state", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn new(viewport: Option<(f64, f64)>) -> Result<Self> {
        let path = cache_dir()
            .context("无法确定缓存目录（XDG_CACHE_HOME/HOME 未设置）")?
            .join("positions.json");
        Ok(DriftSession {
            positions_path: path,
            viewport: viewport.unwrap_or((1920.0, 1080.0)),
        })
    }

    fn fetch_state(&self) -> Result<DriftState> {
        let out = Command::new("driftwm")
            .args(["msg", "state", "--json"])
            .stdin(Stdio::null())
            .output()
            .context("无法运行 driftwm msg state")?;
        let v: Envelope =
            serde_json::from_slice(&out.stdout).context("driftwm msg state 输出不是合法 JSON")?;
        if let Some(err) = v.err {
            bail!("driftwm msg state 失败: {err}");
        }
        v.ok
            .and_then(|p| p.state)
            .context("driftwm msg state 返回缺少 State 字段")
    }

    fn msg(&self, args: &[&str]) -> Result<()> {
        // 吞掉 driftwm 自己的 stdout；stderr 捕获进错误信息，
        // hide 流程靠其中的 "fullscreen" 字样识别全屏窗口
        let out = Command::new("driftwm")
            .arg("msg")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("无法运行 driftwm msg {}", args.join(" ")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("driftwm msg {} 失败: {}", args.join(" "), err.trim());
        }
        Ok(())
    }

    /// 运行一个配置动作（作用于聚焦窗口），如 toggle-fullscreen
    fn action(&self, name: &str) -> Result<()> {
        self.msg(&["action", name])
    }

    /// driftwm IPC 的坐标/尺寸参数只接受整数，f64 一律四舍五入
    fn fmt_i(v: f64) -> String {
        format!("{}", v.round() as i64)
    }

    fn move_to(&self, id: &str, (x, y): (f64, f64)) -> Result<()> {
        self.msg(&["move", "--id", id, &Self::fmt_i(x), &Self::fmt_i(y)])
    }

    fn resize_to(&self, id: &str, (w, h): (f64, f64)) -> Result<()> {
        self.msg(&["resize", "--id", id, &Self::fmt_i(w), &Self::fmt_i(h)])
    }

    /// 聚焦窗口。实测 driftwm 的 `focus --id` 路径对部分客户端（如 emacs）
    /// 不生效，而按 app_id 子串的路径有效，故优先用 app_id；没有 app_id 的
    /// 窗口（如部分 XWayland 窗口）退回 --id。
    fn focus(&self, win: &Win) -> Result<()> {
        if win.app_id.is_empty() {
            self.msg(&["focus", "--id", &win.key])
        } else {
            self.msg(&["focus", &win.app_id])
        }
    }

    fn load_positions(&self) -> Positions {
        std::fs::read_to_string(&self.positions_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_positions(&self, p: &Positions) -> Result<()> {
        if let Some(dir) = self.positions_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.positions_path, serde_json::to_string(p)?)?;
        Ok(())
    }
}

impl Backend for DriftSession {
    fn backend_name(&self) -> &'static str {
        "driftwm"
    }

    fn snapshot(&mut self) -> Result<Vec<Win>> {
        let st = self.fetch_state()?;
        Ok(st
            .windows
            .iter()
            .map(|w| {
                let pos = xy(&w.position);
                Win {
                    key: w.id.to_string(),
                    app_id: w.app_id.clone(),
                    title: w.title.clone(),
                    hidden: in_hide_spot(pos),
                    focused: w.is_focused,
                    position: Some(pos),
                }
            })
            .collect())
    }

    fn hide(&mut self, pad: &Pad, win: &Win) -> Result<()> {
        // 已在藏匿点的窗口跳过：重复 hide 会把真实位置记录覆盖成藏匿点
        if win.hidden {
            return Ok(());
        }
        // 记录当前位置，show 时移回
        if let Some((x, y)) = win.position {
            let mut p = self.load_positions();
            p.0.entry(pad.name.clone())
                .or_default()
                .insert(win.key.clone(), (x, y));
            self.save_positions(&p)?;
        }
        if let Err(e) = self.move_to(&win.key, HIDE_SPOT) {
            // 全屏窗口不能直接移动；state 不反映全屏状态，
            // 只能从 move 的报错识别，退全屏后重试
            if !format!("{e:#}").contains("fullscreen") {
                return Err(e);
            }
            self.focus(win)?;
            self.action("toggle-fullscreen")?;
            self.move_to(&win.key, HIDE_SPOT)?;
        }
        Ok(())
    }

    fn reveal(&mut self, pad: &Pad, win: &Win, focus: bool) -> Result<()> {
        let p = self.load_positions();
        let saved = p
            .0
            .get(&pad.name)
            .and_then(|m| m.get(&win.key))
            .copied();
        let st = self.fetch_state()?;
        let cam = xy(&st.camera);
        let geo = pad.has_geometry();

        if pad.spec.fullscreen {
            // driftwm 的真全屏窗口会脱离画布 inventory（IPC 再也找不到它，
            // 无法继续隐藏/显示），因此以"占满当前视野"实现全屏语义
            let z = st.zoom.max(0.1);
            self.move_to(&win.key, cam)?;
            self.resize_to(&win.key, (self.viewport.0 / z, self.viewport.1 / z))?;
            if focus {
                self.focus(win)?;
            }
            return Ok(());
        }

        // 目标尺寸（屏幕像素）：配置值优先，否则保持当前尺寸
        let cur = st.windows.iter().find(|w| w.id.to_string() == win.key);
        let cur_size = cur.map(|w| xy(&w.size)).unwrap_or((0.0, 0.0));
        let want_resize = pad.spec.width.is_some() || pad.spec.height.is_some();
        let (w_px, h_px) = if geo {
            let z = st.zoom.max(0.1);
            (
                pad.spec.width.map_or(cur_size.0 * z, |v| v as f64),
                pad.spec.height.map_or(cur_size.1 * z, |v| v as f64),
            )
        } else {
            (0.0, 0.0)
        };

        // 目标位置：配置了几何且尺寸可算则按停靠边计算，否则回隐藏前位置
        let target = if geo && w_px > 0.0 && h_px > 0.0 {
            edge_position(
                pad.spec.edge.unwrap_or(Edge::Top),
                pad.spec.margin as f64,
                w_px,
                h_px,
                cam,
                st.zoom,
                self.viewport,
            )
        } else {
            match saved {
                Some(pos) if in_viewport(pos, cam, st.zoom, self.viewport) => pos,
                // 没有隐藏记录，或恢复位置在当前视野之外：带到当前视野中心
                _ => cam,
            }
        };

        self.move_to(&win.key, target)?;
        // driftwm 的 resize 保持窗口中心不变，与 move 互不影响
        if want_resize && cur.is_some() {
            let z = st.zoom.max(0.1);
            self.resize_to(&win.key, (w_px / z, h_px / z))?;
        }
        if focus {
            self.focus(win)?;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        Ok(())
    }

    fn wait_for(&mut self, pad: &Pad, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            let st = self.fetch_state()?;
            if let Some(w) = st
                .windows
                .iter()
                .find(|w| pad.matches(&w.app_id, &w.title))
            {
                let win = Win {
                    key: w.id.to_string(),
                    app_id: w.app_id.clone(),
                    title: w.title.clone(),
                    hidden: false,
                    focused: false,
                    position: Some(xy(&w.position)),
                };
                // 窗口刚出现时 driftwm 的 auto placement 可能还没完成，
                // 稍等再定位，避免与放置逻辑竞态；几何/聚焦统一走 reveal
                std::thread::sleep(PLACEMENT_SETTLE);
                self.reveal(pad, &win, pad.spec.focus_on_show)?;
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn describe(&self, win: &Win) -> String {
        match win.position {
            Some((x, y)) => format!("id={} @({x:.0},{y:.0})", win.key),
            None => win.key.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_reply() {
        let raw = r#"{"Ok":{"State":{"camera":[70.0,359.0],"zoom":1.0,
            "windows":[{"id":7,"app_id":"firefox","title":"t","position":[70,344],
            "size":[1920,1050],"is_focused":true,"is_widget":false,"suspended":false,"mode":"Fit"}]}}}"#;
        let v: Envelope = serde_json::from_str(raw).unwrap();
        let st = v.ok.unwrap().state.unwrap();
        assert_eq!(st.windows.len(), 1);
        assert_eq!(st.windows[0].id, 7);
        assert!(st.windows[0].is_focused);
        assert_eq!(xy(&st.camera), (70.0, 359.0));
    }

    #[test]
    fn parses_err_reply() {
        let v: Envelope = serde_json::from_str(r#"{"Err":"no match"}"#).unwrap();
        assert!(v.ok.is_none());
        assert_eq!(v.err.unwrap(), "no match");
    }

    #[test]
    fn hide_spot_detection() {
        assert!(in_hide_spot(HIDE_SPOT));
        assert!(in_hide_spot((HIDE_SPOT.0 + 100.0, HIDE_SPOT.1 - 100.0)));
        assert!(!in_hide_spot((1067.0, 1187.0)));
        assert!(!in_hide_spot((0.0, 0.0)));
    }

    const VP: (f64, f64) = (1920.0, 1080.0);

    #[test]
    fn edge_position_top_and_bottom() {
        let cam = (100.0, 200.0);
        // top：窗口顶边 = 屏幕顶边 - margin（屏幕坐标 Y-up）
        let (x, y) = edge_position(Edge::Top, 40.0, 800.0, 600.0, cam, 1.0, VP);
        assert_eq!((x, y), (100.0, 200.0 + 540.0 - 40.0 - 300.0));
        // bottom：窗口底边 = 屏幕底边 + margin
        let (x, y) = edge_position(Edge::Bottom, 40.0, 800.0, 600.0, cam, 1.0, VP);
        assert_eq!((x, y), (100.0, 200.0 - 540.0 + 40.0 + 300.0));
    }

    #[test]
    fn edge_position_left_right_center() {
        let cam = (0.0, 0.0);
        let (x, y) = edge_position(Edge::Left, 10.0, 500.0, 500.0, cam, 1.0, VP);
        assert_eq!((x, y), (-960.0 + 10.0 + 250.0, 0.0));
        let (x, y) = edge_position(Edge::Right, 10.0, 500.0, 500.0, cam, 1.0, VP);
        assert_eq!((x, y), (960.0 - 10.0 - 250.0, 0.0));
        let (x, y) = edge_position(Edge::Center, 10.0, 500.0, 500.0, cam, 1.0, VP);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn edge_position_respects_zoom() {
        // zoom=0.5：屏幕 40px 边距 = 画布 80，窗口 600px 高 = 画布 1200，
        // 视野半高 = 1080/0.5 = 1080 画布单位 → 中心 y = 1080-80-600 = 400
        let cam = (0.0, 0.0);
        let (x, y) = edge_position(Edge::Top, 40.0, 800.0, 600.0, cam, 0.5, VP);
        assert_eq!((x, y), (0.0, 400.0));
    }
}
