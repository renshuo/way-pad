# way-pad

Wayland 下的 [tdrop](https://github.com/noctuid/tdrop) 式窗口开关：按配置文件里定义的 **pad**（窗口匹配规则 + 可选启动命令）对指定窗口做显示/隐藏/聚焦切换。把 `way-pad toggle <pad>` 绑到合成器快捷键上，就是典型的"下拉终端/随手音乐播放器"用法。

专为 [driftwm](https://github.com/malbiruk/driftwm) 做了适配，也支持任何实现 `zwlr_foreign_toplevel_management_v1` 的合成器。

## 工作原理

way-pad 通过两个后端隐藏窗口，默认 `backend = "auto"` 自动选择：

| 后端 | 适用合成器 | 隐藏方式 |
|------|-----------|---------|
| `driftwm` | driftwm | 无限画布上没有"最小化"概念（foreign-toplevel 的 `set_minimized` 在 driftwm 上是 no-op），way-pad 通过 `driftwm msg` IPC 把窗口移到画布极远的藏匿点实现隐藏；显示时移回隐藏前记录的位置并聚焦。若恢复位置不在当前视野内，则把窗口带到当前视野中心，保证按快捷键后总能看到它 |
| `foreign-toplevel` | wlroots/smithay 系 | 标准 `zwlr_foreign_toplevel_management_v1` 协议的 minimize/unminimize/activate |

## 安装

```sh
cargo install --path .        # 装到 ~/.cargo/bin
# 或直接用构建产物 target/release/way-pad
```

## 配置

配置文件位于 `~/.config/way-pad/config.toml`（可用 `-c` 或环境变量 `WAY_PAD_CONFIG` 覆盖），完整示例见 [example/config.toml](example/config.toml)：

```toml
# 自动探测后端：driftwm 会话用 driftwm IPC，否则用 foreign-toplevel 协议
backend = "auto"                # auto | driftwm | foreign-toplevel

# 隐藏方式（仅 driftwm 后端）：move = 移到画布藏匿点（默认）；
# opacity = 窗口原地全透明。注意：透明窗口通常仍会拦截鼠标点击
hide_mode = "move"              # move | opacity

# driftwm 后端判断"窗口是否在当前视野内"所用的视口尺寸（显示器分辨率）
viewport = [1920, 1080]

# launch 启动后等待窗口出现的默认毫秒数（可被 pad 级覆盖）
launch_wait_ms = 1500

# 每个 [pads.<名字>] 定义一个 pad。app_id 和 title 都是正则表达式，
# title 省略（或留空）表示不限定；launch 在没有匹配窗口时经 sh -c 执行。
[pads.term]
app_id = 'foot'
launch = 'foot'
focus_on_show = true            # 显示时聚焦（默认 true）
launch_wait_ms = 3000           # pad 级等待时间，覆盖顶层值

# 窗口几何（仅 driftwm 后端支持）：显示时 1600x900、居中；
# 尺寸也可用视口百分比，如 width = "90%"
[pads.emacs]
app_id = '(?i)^emacs$'
launch = 'emacs --name way-pad-emacs'
width = 1600
height = 900
edge = "center"                 # top / bottom / left / right / center

# 下拉式：贴屏幕顶部、留 24px 边距、高 50% 视口，宽度保持窗口当前值
[pads.music]
app_id = '^mpv'
title = '^mpv —'
launch = 'mpv --player-operation-mode=pseudo-gui'
edge = "top"
margin = 24
height = "50%"

# 全屏 pad：显示时占满当前视野
[pads.calc]
app_id = '^gnome-calculator$'
launch = 'gnome-calculator'
fullscreen = true
```

字段说明：

| 字段 | 位置 | 说明 |
|------|------|------|
| `backend` | 顶层 | `auto` / `driftwm` / `foreign-toplevel`，默认 `auto` |
| `hide_mode` | 顶层 | 隐藏方式：`move`（默认）/ `opacity`（仅 driftwm 后端） |
| `viewport` | 顶层 | 显示器分辨率（屏幕像素），driftwm 后端用于视野判断、百分比与全屏尺寸，默认 `[1920, 1080]` |
| `launch_wait_ms` | 顶层 / pad | 自动启动后等待窗口出现的超时，默认 1500；pad 级覆盖顶层 |
| `app_id` | pad | 匹配窗口 app_id 的正则，必填 |
| `title` | pad | 匹配窗口标题的正则，可选 |
| `launch` | pad | 无匹配窗口时启动的命令（`sh -c`），可选；不配置时 toggle 在无窗口时报错 |
| `focus_on_show` | pad | 显示时是否聚焦，默认 `true` |
| `width` / `height` | pad | 显示时的窗口尺寸：像素（`1600`）或视口百分比（`"90%"`）；省略则保持当前尺寸 |
| `edge` | pad | 停靠边：`top` / `bottom` / `left` / `right` / `center`（默认 `top`） |
| `margin` | pad | 距视野边缘的边距（屏幕像素），默认 0 |
| `fullscreen` | pad | 显示时占满当前视野；为 true 时忽略 width/height/edge/margin |

几何行为细节（driftwm 后端）：

- `width`/`height`/`margin` 都是**屏幕像素**（或视口百分比），随当前缩放（zoom）自动换算成画布坐标；
- 显示时窗口总是出现在**当前视野**的对应位置（隐藏期间你移动了视野，窗口也会被带到眼前）；
- **悬浮定位**：driftwm 的 focus 会把相机平移到窗口居中位（约 300ms 动画，无法抑制），若窗口先落到贴边/停靠位，聚焦时视野会往返晃动。way-pad 采用"先对焦、后停靠"——窗口先到视野中心与 focus 目标对齐（相机动画≈0），聚焦完成后再瞬移到最终停靠位，**全程视野稳定、无视图跳跃**（`WAY_PAD_DEBUG=1` 可查看定位日志）；
- `fullscreen` 采用"占满视野"实现——driftwm 的真全屏窗口会脱离画布管理（IPC 无法再找到它），所以 way-pad 不使用真全屏，以保证隐藏/显示循环始终可用；
- 几何与 `hide_mode = "opacity"` 在其他后端（foreign-toplevel）上会被忽略并给出提示。

按键保护：同一 pad 的 way-pad 进程互斥（flock）——快速连按或按键重复时，后到的实例直接退出，不会开出多个窗口。

先跑 `way-pad list` 看当前窗口的 `app_id`/`title` 长什么样，再照着写正则。

## 用法

```sh
way-pad toggle term     # 有可见窗口→隐藏；否则显示并聚焦；无窗口→按 launch 启动
way-pad show term       # 显示并聚焦
way-pad hide term       # 隐藏
way-pad close term      # 关闭 pad 的窗口
way-pad list            # 列出配置的 pad 和当前窗口
way-pad list --json     # 同上，JSON 格式（供脚本消费，不受 -q 影响）
way-pad doctor          # 诊断：后端探测、配置解析、每个 pad 匹配到几个窗口

way-pad toggle          # 配置里只有一个 pad 时可省略名字
way-pad -q toggle term  # -q 安静模式（脚本里用）
way-pad -c other.toml … # 指定配置
way-pad -w 3000 toggle …# 覆盖启动等待时间
```

一个 pad 可以匹配多个窗口（正则写宽一点即可），toggle 会把它们当作一组一起显示/隐藏。

## 绑定到 driftwm 快捷键

在 `~/.config/driftwm/config.toml` 里：

```toml
[keybindings]
"mod+t" = "spawn way-pad toggle term"
"mod+m" = "spawn way-pad toggle music"
```

## 在其他合成器上

任何支持 `zwlr_foreign_toplevel_management_v1` 的合成器都可以用（`backend = "foreign-toplevel"` 或 auto 探测失败时自动回落）。注意部分合成器（如 sway）没有实现最小化概念、也不支持该协议；用 `wayland-info | grep foreign_toplevel` 检查支持情况。

## 退出码

- `0`：成功
- 非 `0`：失败（配置缺失/非法、合成器不支持协议、IPC 失败、已启动但等待超时未出现窗口等），错误信息输出到 stderr

## 开发

```sh
cargo test           # 单元测试
cargo build --release
```

推送时 GitHub Actions 会自动跑 `cargo fmt --check`、`clippy`、`test` 与 `build`。
