//! 优雅关闭信号原语。
//!
//! 基于 `tokio::sync::watch<bool>` 的轻量实现(MVP 选型 A1,零额外依赖):
//! [`ShutdownSender`] 触发停止,可克隆的 [`Shutdown`] 由各并发任务周期性
//! 检查(`is_cancelled`),实现"当前轮次完成后停止,不再启动新轮次"的语义。

use std::time::Duration;

use tokio::sync::watch;

/// 优雅关闭信号(检查端,可克隆)。
///
/// 每个并发任务持有一份:每轮 `run` 开始前检查一次,
/// 信号到达时完成当前轮次(含 validate)后退出,不启动新轮次。
/// 初始化时未取消;由 [`ShutdownSender::cancel`] 或 [`Shutdown::install_ctrl_c`]
/// 触发后永久保持取消态。
#[derive(Clone, Debug)]
pub struct Shutdown {
    rx: watch::Receiver<bool>,
}

/// 优雅关闭触发端。
///
/// 使用者可依业务条件触发(如超时、外部指令),见 [`Shutdown::install_ctrl_c`]
/// 的 OS 信号便捷绑定。可 Clone,多个触发端共享同一信号。
#[derive(Clone, Debug)]
pub struct ShutdownSender {
    tx: watch::Sender<bool>,
}

impl Shutdown {
    /// 创建一对 (触发端, 检查端)。
    pub fn new() -> (ShutdownSender, Shutdown) {
        let (tx, rx) = watch::channel(false);
        (ShutdownSender { tx }, Shutdown { rx })
    }

    /// 是否已收到停止信号。
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Future:等待停止信号;已触发则立即完成。
    ///
    /// 用于退避等待等可中断路径:`tokio::select!` 中与休眠竞争,
    /// 信号到达时立即返回,不阻塞优雅关闭。
    /// 内部以 10ms 粒度轮询(`watch::Receiver::changed` 需可变借用,
    /// 与共享 `Shutdown` 语义冲突),对关闭信号的响应延迟可忽略。
    pub async fn wait_cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 便捷绑定:在 Tokio runtime 中监听 OS 的 Ctrl-C(SIGINT),
    /// 收到信号时自动触发停止。
    ///
    /// 内部 `tokio::spawn` 一个后台任务,须在 Tokio runtime 内调用
    /// (与 `ScenarioManager::run*` 的 `block_on` 调用方式一致)。
    /// 需要 `tokio` 的 `signal` feature(本 crate 已开启)。
    pub fn install_ctrl_c() -> Shutdown {
        let (tx, rx) = Self::new();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tx.cancel();
            }
        });
        rx
    }
}

impl ShutdownSender {
    /// 触发停止:所有 `Shutdown` 持有者(并发任务)在当前轮次结束后退出。
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }
}
