# herdr-espresso ☕

> 只要 herdr 里有任意编码 agent 在工作，就让 Mac 保持唤醒——整个 herdr 一个开关。

[English](README.md) | **简体中文**

![license](https://img.shields.io/badge/license-MIT-blue.svg)
![platform](https://img.shields.io/badge/platform-macOS-lightgrey.svg)

`herdr-espresso` 是一个 [herdr](https://github.com/ogulcancelik/herdr) 插件：只要 herdr
里有任意 agent 正在工作就让 macOS 保持唤醒，全部停下后再让它可以睡眠。它驱动
[`espresso`](https://github.com/Hanyang-Li/espresso) 命令行工具——开启开关后，一个小的、
脱离终端的 watcher 通过 herdr socket 跟踪每个 pane 的 agent 状态，据此持有或释放一个
`espresso` 会话。

- **任一 agent working/blocked → 保持唤醒。** 只要至少有一个 agent 处于活跃状态，Mac
  就不会睡眠（若装了 espresso 的合盖辅助进程，合盖也不睡）。
- **全部 idle/done → 允许睡眠。** 最后一个活跃 agent 停下后，经过一段短暂的宽限期释放
  `espresso`；开关仍保持开启，任一 agent 再次活跃时会重新持有。
- **整个 herdr 一个开关。** 开一次即可，覆盖所有 workspace、tab 和 pane（包括之后新开的），
  herdr 重启后依然生效。

## 特性

- **跟随每个 agent。** 无需按 pane 设置——之后出现的 pane 和 agent 会被自动接管。
- **标签栏状态。** 开关开启时，herdr 标签栏右侧显示 `󰅶 awake` / `󰛊 idle`，关闭时不显示
  ——只需在 herdr 配置里加一行（见[标签栏状态](#标签栏状态)）。
- **记住开关。** 开启后，herdr 重启也会自动恢复为开启。
- **自愈租约。** `espresso` 以短租约方式持有、agent 工作期间不断续租，因此崩溃的
  watcher 绝不会一直占着 Mac 不睡——租约会在 ~90 秒内自动到期。
- **事件驱动。** watcher 空闲时阻塞在内核里、几乎 0% CPU；toggle 关闭是瞬时的。

## 依赖

- **仅 macOS。**
- [herdr](https://github.com/ogulcancelik/herdr) **0.8.2** 或更高版本。
- 已安装并在 `PATH` 中的 [`espresso`](https://github.com/Hanyang-Li/espresso) 命令行工具。
  安装：
  ```sh
  curl -fsSL https://raw.githubusercontent.com/Hanyang-Li/espresso/main/install.sh | sh
  ```
- **可选：** 执行一次 `espresso daemon install`（需要 `sudo`），让 Mac 在**合盖**时
  也保持唤醒。不装则只防空闲/息屏睡眠；开启时会有提醒。

## 安装

```sh
herdr plugin install Hanyang-Li/herdr-espresso
```

herdr 会克隆仓库、执行构建钩子（`cargo build --release`）并注册插件。用 `--ref`
锁定具体版本：

```sh
herdr plugin install Hanyang-Li/herdr-espresso --ref v1.0.0
```

## 键位绑定

插件本身不绑定按键——在你的 herdr 配置里把 `espresso.toggle` action 绑到某个键，例如：

```toml
[[keys.command]]
key = "prefix+/"
type = "plugin_action"
command = "espresso.toggle"
description = "espresso: toggle global keep-awake"
```

在任意位置按下它，即可为整个 herdr 开启或关闭保持唤醒。每次切换都会弹出 toast 提示。
该 action 也会出现在 herdr 的命令面板里，名为 **Espresso: toggle global keep-awake**。

## 标签栏状态

开关开启期间，插件会把状态写进一个小文件，供 herdr 标签栏条目显示。在 `ui.tab_bar_right`
里加一个 command 条目：

```toml
[ui]
tab_bar_right = [
  { type = "command", command = "cat ~/.local/state/herdr/plugins/espresso/statusline", interval_seconds = 2, timeout_seconds = 1 },
]
```

| 显示 | 含义 |
|---|---|
| `󰅶 awake` | 已开启，且至少一个 agent 在 working 或 blocked——Mac 保持唤醒。 |
| `󰛊 idle` | 已开启，所有 agent 都空闲——Mac 可以睡眠。 |
| *（不显示）* | 已关闭。 |

图标是 Nerd Font 字形（md-coffee、md-coffee_outline），终端需使用 Nerd Font。如果设置了
`XDG_STATE_HOME`，该文件则位于 `$XDG_STATE_HOME/herdr/plugins/espresso/statusline`。
其他条目（`datetime`、`hostname` 等）可以放在同一个列表里。

### 从 0.1 升级

0.1 逐个 pane 监控，并用 `$espresso` token 在侧边栏给它们打上记号。1.0 改为全局开关：首次运行
1.0 时会自动停掉旧的按 pane watcher，你可以从 `[ui.sidebar.agents] rows`（以及任何
`rows_by_agent` 条目）里删掉 `$espresso`。
1.0 还移除了 `espresso.status` 动作：请删掉绑定到它的快捷键，否则 herdr 会在
`herdr server reload-config` 的诊断信息里报告它。

## 工作原理

- **一个 watcher。** 开启开关会启动一个脱离终端的 watcher。它订阅 herdr 的 pane 生命周期
  事件以及每个 pane 的 agent 状态事件，一有变化就重新读取 pane 列表（`pane.list`）；
  pane 增减时会重建订阅。
- **任一 working/blocked → 唤醒。** watcher 用 90 秒的短租约持有 `espresso`，只要还有
  agent 处于活跃状态就每 60 秒续租（轮换）一次，因此保持无空档。
- **全部 idle/done → 宽限后释放。** 没有 agent 活跃后，watcher 等约 5 秒（避免瞬时闪回
  working 反复起停）再释放 `espresso`。开关保持开启。
- **自愈租约。** 因为租约短且靠续租维持，watcher 若意外死亡（崩溃、`kill -9`）也无法
  一直占着 Mac 不睡——其 `espresso` 租约会在 ~90 秒内到期。
- **重启安全。** 开关记录在 herdr 的插件状态目录里；herdr server 启动时，插件的启动钩子
  会把 watcher 拉起来。detach 掉客户端不受影响；`herdr server stop` 会结束它。
  toggle 与启动都会先获取一把锁（`watcher.lock`），因此不会互相竞争。

## 卸载

```sh
herdr plugin uninstall Hanyang-Li/herdr-espresso
```

## 开发

本地开发、或想在发布前试用改动时，用 **link** 挂载一份工作副本，而不是从 GitHub 安装：

```sh
git clone https://github.com/Hanyang-Li/herdr-espresso
cd herdr-espresso
herdr plugin link "$PWD"      # 就地执行构建钩子并注册
```

直接构建与测试：

```sh
cargo build --release
cargo test
```

推送 `v*` tag 时，GitHub Actions 会自动构建并发布 release（见
`.github/workflows/release.yml`）。

## 许可证

[MIT](LICENSE) © Hanyang Li
