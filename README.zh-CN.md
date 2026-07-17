# herdr-espresso ☕

> 当 herdr 某个 pane 里的 AI 编码 agent 在工作时，自动让 Mac 保持唤醒——按 pane 独立生效。

[English](README.md) | **简体中文**

![license](https://img.shields.io/badge/license-MIT-blue.svg)
![platform](https://img.shields.io/badge/platform-macOS-lightgrey.svg)

`herdr-espresso` 是一个 [herdr](https://github.com/ogulcancelik/herdr) 插件：当被监控
pane 里的编码 agent 正在工作时让 macOS 保持唤醒，agent 空闲后再让它可以睡眠。它驱动
[`espresso`](https://github.com/Hanyang-Li/espresso) 命令行工具——每当你对一个 pane
开启监控，就有一个小的、脱离终端的 watcher 通过 herdr socket 订阅该 pane 的 agent 状态
事件，据此持有或释放一个 `espresso` 会话。

- **working/blocked → 保持唤醒。** agent 活跃期间 Mac 不会睡眠（若装了 espresso 的
  合盖辅助进程，合盖也不睡）。
- **idle/done → 允许睡眠。** agent 停下后，经过一段短暂的宽限期释放 `espresso`；该
  pane 仍处于监控中，agent 再次活跃时会重新持有。
- **按 pane、全自动。** 每个 pane 开一次即可；多个 pane 各自独立监控。pane 关闭、或
  agent 退出，都会自动停止该 pane 的监控。

## 特性

- **跟随 agent 状态。** 无需手动开关，监控随 agent 状态自动生效。
- **仅 agent pane。** 对纯 shell 开启监控会被拒绝并弹通知（shell 没有工作/空闲状态可
  跟踪）。
- **侧边栏标记。** 被监控的 pane 旁会显示 `espresso` 标记，停止监控后消失。
- **自愈租约。** `espresso` 以短租约方式持有、agent 工作期间不断续租，因此崩溃的
  watcher 绝不会一直占着 Mac 不睡——租约会在 ~90 秒内自动到期。
- **事件驱动。** watcher 空闲时阻塞在内核里、几乎 0% CPU；toggle 关闭是瞬时的。

## 依赖

- **仅 macOS。**
- [herdr](https://github.com/ogulcancelik/herdr) **0.7.0** 或更高版本。
- 已安装并在 `PATH` 中的 [`espresso`](https://github.com/Hanyang-Li/espresso) 命令行工具。
  安装：
  ```sh
  curl -fsSL https://raw.githubusercontent.com/Hanyang-Li/espresso/main/install.sh | sh
  ```
- **可选：** 执行一次 `espresso daemon install`（需要 `sudo`），让 Mac 在**合盖**时
  也保持唤醒。不装则只防空闲/息屏睡眠；开启监控时会有一次性提醒。

## 安装

```sh
herdr plugin install Hanyang-Li/herdr-espresso
```

herdr 会克隆仓库、执行构建钩子（`cargo build --release`）并注册插件。用 `--ref`
锁定具体版本：

```sh
herdr plugin install Hanyang-Li/herdr-espresso --ref v0.1.0
```

## 键位绑定

插件本身不绑定按键——在你的 herdr 配置里把 `espresso.toggle` action 绑到某个键，例如：

```toml
[[keys.command]]
key = "prefix+/"
type = "plugin_action"
command = "espresso.toggle"
description = "espresso: toggle monitor on focused pane"
```

焦点在一个 **agent** pane 上时按下它，即可对该 pane 开/关监控。监控期间该 pane 侧边栏
会显示 `espresso` 标记。对没有 agent 的 pane 按下会被拒绝并弹通知。

## 工作原理

- **working/blocked → 唤醒。** watcher 用 90 秒的短租约持有 `espresso`，agent 活跃
  期间每 60 秒续租（轮换）一次，因此保持无空档。
- **idle/done → 宽限后释放。** agent 停止活跃后，watcher 等约 5 秒（避免瞬时闪回
  working 反复起停）再释放 `espresso`，监控本身继续。
- **agent 退出 / pane 关闭 → 停止。** 若 pane 关闭，或 agent 退出、pane 变回纯 shell，
  watcher 会自行停止：释放 `espresso` 并移除标记。
- **自愈租约。** 因为租约短且靠续租维持，watcher 若意外死亡（崩溃、`kill -9`）也无法
  一直占着 Mac 不睡——其 `espresso` 租约会在 ~90 秒内到期。
- **detach 安全。** watcher 以独立 session 脱离运行，detach 掉 herdr 客户端不会停止
  监控；`herdr server stop` 会清理它。
- **按 pane 独立。** 每个被监控 pane 有各自的 watcher 和各自的 `espresso`，互不影响。

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
