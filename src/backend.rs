//! 后端抽象：一个后端负责枚举窗口、隐藏/显示/聚焦。
//!
//! - `foreign-toplevel`：标准 `zwlr_foreign_toplevel_management_v1`（wlroots/smithay 系）
//! - `driftwm`：driftwm 的 IPC（无限画布合成器，没有最小化概念，用"移出视野"实现隐藏）

use crate::config::Pad;
use anyhow::Result;
use std::time::Duration;

/// 窗口快照
#[derive(Debug, Clone)]
pub struct Win {
    /// 后端内部标识（driftwm 的稳定 id；foreign-toplevel 后端留空）
    pub key: String,
    pub app_id: String,
    pub title: String,
    /// 已隐藏（已最小化，或已移到画布藏匿点）
    pub hidden: bool,
    pub focused: bool,
    /// 画布坐标（visible-frame 中心，Y-up）；仅 driftwm 后端提供
    pub position: Option<(f64, f64)>,
}

pub trait Backend {
    fn backend_name(&self) -> &'static str;

    /// 当前所有窗口的快照
    fn snapshot(&mut self) -> Result<Vec<Win>>;

    /// 隐藏单个窗口
    fn hide(&mut self, pad: &Pad, win: &Win) -> Result<()>;

    /// 显示单个窗口；focus 时同时请求聚焦。
    /// 返回窗口最终应处的位置（driftwm 后端用于 launch 后校验定位）
    fn reveal(&mut self, pad: &Pad, win: &Win, focus: bool) -> Result<(f64, f64)>;

    /// 把 hide/reveal 已发出的请求冲到合成器
    fn sync(&mut self) -> Result<()>;

    /// 启动命令后等待匹配 pad 的窗口出现；找到即显示并聚焦，返回 true
    fn wait_for(&mut self, pad: &Pad, timeout: Duration) -> Result<bool>;

    /// 供 list 展示的补充说明（如窗口 id）
    fn describe(&self, win: &Win) -> String {
        win.key.clone()
    }
}
