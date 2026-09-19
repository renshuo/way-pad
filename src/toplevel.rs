//! 通过 `zwlr_foreign_toplevel_management_v1` 协议枚举并控制窗口。
//!
//! driftwm 以及 wlroots/smithay 系合成器都支持该协议；最小化/还原/激活
//! 分别对应 way-pad 的隐藏/显示/聚焦。

use crate::backend::{Backend, Win};
use crate::config::Pad;
use anyhow::{Context, Result};
use std::time::{Duration, Instant};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, State as TlFlag, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

/// way-pad 用到的协议能力都在 v1..=v3 内
const MANAGER_MAX_VERSION: u32 = 3;

#[derive(Debug, Clone)]
pub struct Toplevel {
    pub handle: ZwlrForeignToplevelHandleV1,
    pub app_id: String,
    pub title: String,
    pub minimized: bool,
    pub activated: bool,
    /// 初始属性（title/app_id/state）是否已报告完整
    pub done: bool,
    pub closed: bool,
}

#[derive(Default)]
struct SessionState {
    toplevels: Vec<Toplevel>,
    seat: Option<wl_seat::WlSeat>,
}

pub struct Session {
    queue: EventQueue<SessionState>,
    state: SessionState,
    manager: Option<ZwlrForeignToplevelManagerV1>,
}

impl Session {
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env()
            .context("无法连接 Wayland 合成器（WAYLAND_DISPLAY 未设置？）")?;
        let (globals, queue) =
            registry_queue_init::<SessionState>(&conn).context("registry 初始化失败")?;
        let qh = queue.handle();

        let mut manager = None;
        let mut seat = None;
        for g in globals.contents().clone_list() {
            match g.interface.as_str() {
                "zwlr_foreign_toplevel_manager_v1" => {
                    manager = Some(
                        globals
                            .bind(&qh, 1..=MANAGER_MAX_VERSION, ())
                            .context("绑定 zwlr_foreign_toplevel_manager_v1 失败")?,
                    );
                }
                "wl_seat" => {
                    // 只需要一个能用于 activate() 的 seat，v1 即可
                    seat = Some(globals.bind(&qh, 1..=1, ()).context("绑定 wl_seat 失败")?);
                }
                _ => {}
            }
        }

        Ok(Session {
            queue,
            state: SessionState {
                toplevels: Vec::new(),
                seat,
            },
            manager,
        })
    }

    pub fn supported(&self) -> bool {
        self.manager.is_some()
    }

    /// 阻塞往返，直到所有窗口都报告完初始状态（或轮次用尽）
    pub fn collect(&mut self) -> Result<()> {
        for _ in 0..10 {
            self.queue.roundtrip(&mut self.state)?;
            let settled =
                !self.state.toplevels.is_empty() && self.state.toplevels.iter().all(|t| t.done);
            if settled {
                return Ok(());
            }
        }
        Ok(())
    }

    /// 周期性等待 pred 成立或超时，返回 pred 是否成立
    pub fn wait_until(
        &mut self,
        timeout: Duration,
        pred: impl Fn(&[Toplevel]) -> bool,
    ) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if pred(&self.state.toplevels) {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            self.queue.roundtrip(&mut self.state)?;
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    pub fn live(&self) -> impl Iterator<Item = &Toplevel> {
        self.state.toplevels.iter().filter(|t| !t.closed)
    }

    /// 还原窗口；`activate` 时再请求合成器聚焦
    pub fn restore(&self, t: &Toplevel, activate: bool) {
        t.handle.unset_minimized();
        if activate {
            if let Some(seat) = &self.state.seat {
                t.handle.activate(seat);
            }
        }
    }

    /// 把已发出的请求冲到合成器并等它处理完
    pub fn flush(&mut self) -> Result<()> {
        self.queue.roundtrip(&mut self.state)?;
        Ok(())
    }

    fn find_by_key(&self, key: &str) -> Option<&Toplevel> {
        self.state
            .toplevels
            .iter()
            .find(|t| t.handle.id().protocol_id().to_string() == key)
    }
}

impl Backend for Session {
    fn backend_name(&self) -> &'static str {
        "foreign-toplevel"
    }

    fn snapshot(&mut self) -> Result<Vec<Win>> {
        self.collect()?;
        Ok(self
            .live()
            .map(|t| Win {
                key: t.handle.id().protocol_id().to_string(),
                app_id: t.app_id.clone(),
                title: t.title.clone(),
                hidden: t.minimized,
                focused: t.activated,
                position: None,
            })
            .collect())
    }

    fn hide(&mut self, _pad: &Pad, win: &Win) -> Result<()> {
        if let Some(t) = self.find_by_key(&win.key) {
            t.handle.set_minimized();
        }
        Ok(())
    }

    fn reveal(&mut self, _pad: &Pad, win: &Win, focus: bool) -> Result<(f64, f64)> {
        if let Some(t) = self.find_by_key(&win.key) {
            t.handle.unset_minimized();
            if focus {
                if let Some(seat) = &self.state.seat {
                    t.handle.activate(seat);
                }
            }
        }
        Ok((0.0, 0.0))
    }

    fn close(&mut self, win: &Win) -> Result<()> {
        if let Some(t) = self.find_by_key(&win.key) {
            t.handle.close();
        }
        self.flush()
    }

    fn sync(&mut self) -> Result<()> {
        self.flush()
    }

    fn wait_for(&mut self, pad: &Pad, timeout: Duration) -> Result<bool> {
        let found = self.wait_until(timeout, |ts| {
            ts.iter()
                .any(|t| !t.closed && pad.matches(&t.app_id, &t.title))
        })?;
        if !found {
            return Ok(false);
        }
        let t = self
            .state
            .toplevels
            .iter()
            .find(|t| !t.closed && pad.matches(&t.app_id, &t.title))
            .cloned();
        if let Some(t) = t {
            self.restore(&t, pad.spec.focus_on_show);
            self.flush()?;
        }
        Ok(true)
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for SessionState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // 动态增删的全局对一次性任务无影响，忽略；初始列表由
        // registry_queue_init 自动记录
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for SessionState {
    // toplevel 事件会创建新的 handle 对象，必须告诉 wayland-client 如何初始化它
    wayland_client::event_created_child!(SessionState, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);

    fn event(
        state: &mut Self,
        _manager: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(Toplevel {
                handle: toplevel,
                app_id: String::new(),
                title: String::new(),
                minimized: false,
                activated: false,
                done: false,
                closed: false,
            });
        }
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for SessionState {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;

        let Some(t) = state.toplevels.iter_mut().find(|t| t.handle == *handle) else {
            return;
        };
        match event {
            Event::Title { title } => t.title = title,
            Event::AppId { app_id } => t.app_id = app_id,
            // state 事件携带完整状态数组，覆盖式解析
            Event::State { state: bytes } => {
                t.minimized = false;
                t.activated = false;
                for chunk in bytes.chunks_exact(4) {
                    let v = u32::from_ne_bytes(chunk.try_into().unwrap());
                    match WEnum::from(v) {
                        WEnum::Value(TlFlag::Minimized) => t.minimized = true,
                        WEnum::Value(TlFlag::Activated) => t.activated = true,
                        _ => {}
                    }
                }
            }
            Event::Done => t.done = true,
            Event::Closed => t.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for SessionState {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
