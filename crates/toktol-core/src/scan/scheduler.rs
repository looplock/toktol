//! 自动扫描循环：常驻 Rust 壳的调度器，与窗口生死无关（低功耗销毁窗口后扫描
//! 照常进行）。退避规则是纯函数，与旧前端口径一致：30s 基准，空扫/失败翻倍
//! 封顶 8 分钟，扫到新数据复位——规则只有这一份实现，前端不再有副本。
//!
//! 手动触发与自动轮走同一条通道：等待期收到触发立即开跑；**在飞期间**收到
//! 触发则记为"下一轮立即跑"（比旧前端"忽略"多保留一次用户意图），绝不产生
//! 并发扫描——循环内部单线程。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::Error;
use crate::storage::Storage;

use super::ScanReport;

/// 基准间隔 30 秒。
pub const BASE_INTERVAL_MS: u64 = 30_000;
/// 空扫/失败退避的封顶间隔（8 分钟）。
pub const BACKOFF_CAP_MS: u64 = 8 * 60_000;

/// 下一轮间隔：扫到新数据复位到基准；空扫翻倍，封顶 8 分钟。
/// 失败不走这里（拿不到报告），调用方按翻倍封顶处理。
pub fn next_interval_ms(current_ms: u64, report: &ScanReport) -> u64 {
    let found_data = report.sessions_created > 0 || report.records_inserted > 0;
    if found_data {
        BASE_INTERVAL_MS
    } else {
        (current_ms.saturating_mul(2)).min(BACKOFF_CAP_MS)
    }
}

/// 失败或打不开库时的下一轮间隔：翻倍封顶（与空扫同待遇）。
pub fn next_interval_after_failure(current_ms: u64) -> u64 {
    (current_ms.saturating_mul(2)).min(BACKOFF_CAP_MS)
}

/// 调度器对外事件：壳把它转成 IPC 事件给前端（Started/Finished 各一条）。
#[derive(Debug)]
pub enum ScanEvent {
    /// 一轮开始（自动或手动）。
    Started,
    /// 一轮结束；失败也走这里（退避后重试，循环不退出）。
    Finished(Result<ScanReport, Error>),
}

/// 调度器的构造参数簇：开库工厂、禁用清单与事件回调都由壳注入。
///
/// 生命周期：[`ScanLoopHandle::shutdown`] 之前线程一直存活；每轮结束后照常
/// 排下一轮，`shutdown` 使线程在当前轮跑完（或等待中）退出。
pub struct ScanLoopDeps {
    /// 每轮现开一个库连接（与命令路径同一口径：壳进程里没有长连接的必要）。
    pub storage_factory: Box<dyn Fn() -> Result<Storage, Error> + Send + 'static>,
    /// 禁用工具清单，每轮现读（前端开关变更即时生效，无需重启循环）。
    pub disabled: Box<dyn Fn() -> Vec<String> + Send + Sync + 'static>,
    /// 事件回调：壳注入"发 IPC 事件"的闭包。回调在循环线程上执行，不得阻塞。
    pub on_event: Box<dyn Fn(ScanEvent) + Send + Sync + 'static>,
}

struct LoopState {
    /// 手动触发位：等待期收到即提前醒来开跑；在飞期间置位则下一轮不等间隔。
    trigger: bool,
    /// 退出位：置位后线程在当前轮跑完即返回。
    stop: bool,
}

/// 共享控制面：handle（触发/停）与循环线程各持一半。
struct LoopControl {
    state: Mutex<LoopState>,
    signal: Condvar,
}

/// 运行中的扫描循环。Drop 时静默停线程（壳退出场景）；主动停用 [`shutdown`]。
pub struct ScanLoopHandle {
    control: Arc<LoopControl>,
    stopped: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl ScanLoopHandle {
    /// 手动触发一轮：在飞则记为下一轮立即跑。
    pub fn trigger_now(&self) {
        let mut state = self.control.state.lock().expect("扫描循环状态锁中毒");
        state.trigger = true;
        self.control.signal.notify_all();
    }

    /// 停止循环：当前轮跑完（或等待被打断）后线程返回。重复调用无害。
    pub fn shutdown(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.control.state.lock().expect("扫描循环状态锁中毒").stop = true;
        self.control.signal.notify_all();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// 不等待的停止请求：只置退出位。用于应用退出路径——菜单事件跑在主线程，
    /// join 可能要等扫描整轮跑完，不该阻塞；进程随即退出，轮内半截事务由
    /// WAL 回滚兜底。
    pub fn stop_soon(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.control.state.lock().expect("扫描循环状态锁中毒").stop = true;
        self.control.signal.notify_all();
    }
}

impl Drop for ScanLoopHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 启动扫描循环。首轮在基准间隔后跑（与旧前端一致：页面挂载自己拉数据，
/// 不靠首轮扫描）；手动触发随时可提前。
pub fn spawn(deps: ScanLoopDeps) -> ScanLoopHandle {
    let control = Arc::new(LoopControl {
        state: Mutex::new(LoopState {
            trigger: false,
            stop: false,
        }),
        signal: Condvar::new(),
    });
    let stopped = Arc::new(AtomicBool::new(false));
    let thread_control = Arc::clone(&control);
    let thread_stopped = Arc::clone(&stopped);
    let join = thread::spawn(move || run_loop(thread_control, thread_stopped, deps));
    ScanLoopHandle {
        control,
        stopped,
        join: Some(join),
    }
}

fn run_loop(control: Arc<LoopControl>, stopped: Arc<AtomicBool>, deps: ScanLoopDeps) {
    let mut interval_ms = BASE_INTERVAL_MS;
    loop {
        // 等待：到点、手动触发或退出，三者任一即出循环。
        {
            let deadline = Instant::now() + Duration::from_millis(interval_ms);
            let mut state = control.state.lock().expect("扫描循环状态锁中毒");
            loop {
                if state.stop {
                    return;
                }
                if state.trigger {
                    state.trigger = false;
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let (guard, _) = control
                    .signal
                    .wait_timeout(state, deadline - now)
                    .expect("扫描循环等待中毒");
                state = guard;
            }
        }

        (deps.on_event)(ScanEvent::Started);
        let result = (deps.storage_factory)()
            .and_then(|storage| super::run_scan(&storage, &(deps.disabled)()));
        interval_ms = match &result {
            Ok(report) => next_interval_ms(interval_ms, report),
            Err(_) => next_interval_after_failure(interval_ms),
        };
        (deps.on_event)(ScanEvent::Finished(result));

        // shutdown 在本轮执行期间被调：立即退出而不是再等一轮。
        if stopped.load(Ordering::Relaxed) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicU32;

    fn report(records: u32) -> ScanReport {
        ScanReport {
            records_inserted: records,
            ..ScanReport::default()
        }
    }

    #[test]
    fn backoff_matches_the_old_frontend_rule() {
        let empty = report(0);
        assert_eq!(next_interval_ms(30_000, &empty), 60_000);
        assert_eq!(next_interval_ms(60_000, &empty), 120_000);
        assert_eq!(next_interval_ms(BACKOFF_CAP_MS, &empty), BACKOFF_CAP_MS);
        // 翻倍封顶，不溢出。
        assert_eq!(
            next_interval_after_failure(BACKOFF_CAP_MS * 4),
            BACKOFF_CAP_MS
        );
        // 扫到新记录或新会话都复位。
        assert_eq!(
            next_interval_ms(BACKOFF_CAP_MS, &report(1)),
            BASE_INTERVAL_MS
        );
        assert_eq!(
            next_interval_ms(
                BACKOFF_CAP_MS,
                &ScanReport {
                    sessions_created: 3,
                    ..ScanReport::default()
                }
            ),
            BASE_INTERVAL_MS
        );
    }

    /// 循环整测：真实线程 + 注入依赖。禁用清单禁掉全部工具（不碰本机真实
    /// 日志），工厂首轮失败、之后开真库；验证触发即跑、失败退避后下一轮
    /// 照常成功、shutdown 干净返回。首轮用手动触发，免等 30s 基准。
    #[test]
    fn loop_runs_on_trigger_survives_failure_and_stops() {
        static FACTORY_CALLS: AtomicU32 = AtomicU32::new(0);
        let events: Arc<StdMutex<Vec<String>>> = Arc::default();
        let thread_events = Arc::clone(&events);

        let deps = ScanLoopDeps {
            storage_factory: Box::new(|| {
                if FACTORY_CALLS.fetch_add(1, Ordering::Relaxed) == 0 {
                    return Err(Error::Internal("boom".into()));
                }
                let path = std::env::temp_dir().join(format!(
                    "toktol-scheduler-{}-{}.db",
                    std::process::id(),
                    FACTORY_CALLS.load(Ordering::Relaxed)
                ));
                let _ = std::fs::remove_file(&path);
                crate::storage::open(&path)
            }),
            disabled: Box::new(|| {
                crate::adapter::adapters()
                    .iter()
                    .map(|a| a.tool().as_str().to_string())
                    .collect()
            }),
            on_event: Box::new(move |event| {
                thread_events.lock().unwrap().push(format!("{event:?}"));
            }),
        };
        let mut handle = spawn(deps);
        handle.trigger_now();
        wait_for(
            &events,
            |e| e.iter().any(|e| e.contains("boom")),
            "首轮失败事件",
        );
        // 失败后循环还活着：再次触发，工厂开真库，收到成功完成。
        handle.trigger_now();
        wait_for(
            &events,
            |e| e.iter().any(|e| e.contains("Ok")),
            "后续成功事件",
        );
        handle.shutdown();
        // shutdown 已 join：再触发无害（线程已退出，标志位无人消费）。
        handle.trigger_now();
    }

    fn wait_for(events: &StdMutex<Vec<String>>, pred: impl Fn(&[String]) -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if pred(&events.lock().unwrap()) {
                return;
            }
            assert!(Instant::now() < deadline, "等待 {what} 超时");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
