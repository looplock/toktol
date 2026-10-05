//! Toktol 桌面壳。
//!
//! 纪律：这里**只允许**做窗口、托盘与命令的薄封装。
//! 任何扫描、定价、存储、网关的逻辑都必须先写进 `toktol-core` / `toktol-gateway`，
//! 再由这里的命令转调（扫描循环的调度器在 `toktol-core::scan::scheduler`，这里只注入依赖）。

mod commands;
mod shell_config;

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use toktol_core::scan::scheduler::{ScanEvent, ScanLoopDeps, ScanLoopHandle};
use toktol_gateway::proxy::GatewayHandle;

use shell_config::ShellConfigState;

/// 扫描事件：常驻循环的进度广播给前端（窗口销毁后没有监听者，emit 是无害的）。
const SCAN_STARTED_EVENT: &str = "scan://started";
const SCAN_FINISHED_EVENT: &str = "scan://finished";
const TRAY_ID: &str = "main";
const WINDOW_LABEL: &str = "main";

/// 扫描循环句柄槽位：退出时要取出 shutdown，所以套 Option。
type ScanLoopSlot = Mutex<Option<ScanLoopHandle>>;

/// 转录索引建站的取消旗标槽位。
struct TranscriptBuildSlot(std::sync::Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>);

/// 托盘菜单项句柄：命令要用它改勾选态与文案（跟随界面语言），存 manage 里。
/// 只在默认运行时（Wry）下使用，不泛型化。
struct TrayMenu {
    show: MenuItem<tauri::Wry>,
    scan: MenuItem<tauri::Wry>,
    low_power: CheckMenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}

/// 组装并启动应用：注册插件、注册命令、跑主事件循环。
///
/// 窗口尺寸/标题等静态配置在 `tauri.conf.json`（1080×720，最小 960×640，标题 Toktol；
/// `visible:false`，setup 无条件显示——低功耗只在会话内生效，不跨启动记忆）。
pub fn run() {
    tauri::Builder::default()
        // 单实例：第二个进程把已有窗口唤到前台（销毁态则重建），而不是再开一个
        // 写同一个 SQLite 库的进程。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            exit_low_power(app);
        }))
        // 开机自启。macOS 用 LaunchAgent；Windows/Linux 该参数被忽略（写注册表/桌面文件）。
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // 会话详情图片附件的"打开"：opener 的 open_path 权限在能力文件里
        // 限定为 WorkBuddy 的两个附件目录，其他路径一律拒绝。
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::app_version,
            commands::scan_trigger,
            commands::scan_backlog,
            commands::overview,
            commands::usage_records_page,
            commands::usage_filter_options,
            commands::sessions_page,
            commands::delete_session,
            commands::delete_sessions,
            commands::session_transcript_page,
            commands::session_transcript_turns,
            commands::session_transcript_build,
            commands::session_transcript_cancel,
            commands::tool_config,
            commands::tool_config_entry,
            commands::gateway_status,
            commands::gateway_start,
            commands::gateway_stop,
            commands::gateway_issue_token,
            commands::gateway_overview,
            commands::gateway_requests,
            commands::gateway_upstream_add,
            commands::gateway_upstream_update,
            commands::gateway_upstream_delete,
            commands::gateway_mapping_add,
            commands::gateway_mapping_delete,
            commands::sync_model_catalog,
            commands::pricing_overview,
            commands::set_model_price,
            commands::set_model_mapping,
            commands::bind_variants,
            commands::confirm_model,
            commands::merge_models,
            commands::rename_model,
            commands::shell_get_config,
            commands::shell_set_disabled_tools,
            commands::tray_set_enabled,
            commands::tray_set_texts
        ])
        // 网关句柄槽位：起停命令经它持有运行中的服务。tauri 的 async Mutex
        //（tokio 之上的封装）让 start 持锁跨 await，顺带串行化起停。
        .manage(tauri::async_runtime::Mutex::<Option<GatewayHandle>>::new(
            None,
        ))
        // 配置文件写锁：上游/映射增删改串行化（async Mutex 与句柄槽位同一封装）。
        .manage(tauri::async_runtime::Mutex::<()>::new(()))
        // 转录索引建站的取消旗标槽位（同一时刻至多一个建站任务）。
        .manage(TranscriptBuildSlot(std::sync::Mutex::new(None)))
        .on_window_event(|window, event| {
            // X 键在托盘开启时不是退出：进托盘（低功耗，窗口真销毁省 webview
            // 内存），扫描循环在壳里照常跑。托盘关闭时放行默认行为（真退出）。
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() != WINDOW_LABEL {
                    return;
                }
                let app = window.app_handle();
                if !app.state::<ShellConfigState>().get().tray {
                    return;
                }
                api.prevent_close();
                enter_low_power(app);
            }
        })
        .setup(|app| {
            // 壳偏好先行：托盘显隐与低功耗（开机是否无窗）在启动早期就要定
            // （前端 localStorage 此时不可达），所以落 config.json 而不是 localStorage。
            let state = ShellConfigState::new(
                Some(shell_config_path()),
                shell_config::ShellConfig::load(&shell_config_path()),
            );
            app.manage(state.clone());
            spawn_scan_loop(app.handle().clone(), state.clone());
            build_tray(app.handle().clone())?;
            // 窗口配置成不可见创建，这里再决定去留：低功耗只在会话内生效，
            // 启动一律按非低功耗打开（顺带清掉上次会话可能残留的落盘状态）。
            state.update(|c| c.low_power = false);
            sync_low_power_item(app.handle(), false);
            if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
                let _ = window.show();
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("启动 Toktol 主窗口失败")
        .run(|app, event| {
            // 低功耗销毁了最后一个窗口时 tauri 会请求退出（code None）：托盘
            // 还在且扫描循环常驻，拒绝退出。显式退出（托盘"退出"）带 code，放行。
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
                && app.state::<ShellConfigState>().get().tray
            {
                api.prevent_exit();
            }
        })
}

/// 壳配置路径：数据目录不可得（无 HOME）几乎不可能，但托盘偏好总要有个去处——
/// 退到临时目录只丢偏好持久化，不碰数据红线。
fn shell_config_path() -> PathBuf {
    toktol_core::paths::shell_config_path()
        .unwrap_or_else(|| std::env::temp_dir().join(toktol_core::paths::SHELL_CONFIG_FILE))
}

/// 启动常驻扫描循环：与窗口生死无关，低功耗销毁窗口后照常扫描。
fn spawn_scan_loop(app: AppHandle, state: ShellConfigState) {
    let event_app = app.clone();
    let deps = ScanLoopDeps {
        storage_factory: Box::new(|| {
            let db = toktol_core::paths::main_db_path()
                .ok_or_else(|| toktol_core::error::Error::HomeDirUnavailable)?;
            toktol_core::storage::open(&db)
        }),
        disabled: Box::new(move || state.get().disabled_tools),
        on_event: Box::new(move |event| match event {
            ScanEvent::Started => {
                let _ = event_app.emit(SCAN_STARTED_EVENT, ());
            }
            ScanEvent::Finished(Ok(report)) => {
                let _ = event_app.emit(
                    SCAN_FINISHED_EVENT,
                    serde_json::json!({ "ok": true, "report": report }),
                );
            }
            ScanEvent::Finished(Err(err)) => {
                let _ = event_app.emit(
                    SCAN_FINISHED_EVENT,
                    serde_json::json!({ "ok": false, "error": err.code().as_str() }),
                );
            }
        }),
    };
    app.manage(Mutex::new(Some(toktol_core::scan::scheduler::spawn(deps))));
}

/// 触发一轮扫描（托盘"立即扫描"与前端手动按钮共用）。
fn trigger_scan(app: &AppHandle) -> Result<(), String> {
    let slot = app.state::<ScanLoopSlot>();
    let guard = slot.lock().expect("扫描循环槽位锁中毒");
    guard
        .as_ref()
        .ok_or_else(|| "扫描循环未运行".to_string())?
        .trigger_now();
    Ok(())
}

/// 主窗口唤起（不存在则按配置重建——低功耗销毁后的唯一恢复路径）。
fn ensure_main_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == WINDOW_LABEL)
        .cloned()
        .ok_or_else(|| "窗口配置缺失".to_string())?;
    let window = tauri::WebviewWindowBuilder::from_config(app, &config)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let _ = window.show();
    let _ = window.set_focus();
    Ok(())
}

/// 进入低功耗：主窗口真销毁（省 webview 内存），仅托盘常驻，扫描照常。
fn enter_low_power(app: &AppHandle) {
    app.state::<ShellConfigState>()
        .update(|c| c.low_power = true);
    sync_low_power_item(app, true);
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.destroy();
    }
}

/// 退出低功耗：重建/唤起主窗口，勾选与偏好同步。
fn exit_low_power(app: &AppHandle) {
    app.state::<ShellConfigState>()
        .update(|c| c.low_power = false);
    sync_low_power_item(app, false);
    if let Err(err) = ensure_main_window(app) {
        eprintln!("重建主窗口失败: {err}");
    }
}

/// 托盘无条件创建、可见性按壳偏好：运行时开关走 `set_visible`，免掉"关了再开
/// 要重建菜单/事件绑定"的麻烦。菜单文案是中文缺省——前端起来后会按当前界面
/// 语言经 `tray_set_texts` 校正，这里保证前端未起或纯托盘场景也有可读菜单。
fn build_tray(app: AppHandle) -> tauri::Result<()> {
    let visible = app.state::<ShellConfigState>().get().tray;
    let low_power = app.state::<ShellConfigState>().get().low_power;
    let show = MenuItem::with_id(&app, "show", "显示主窗口", true, None::<&str>)?;
    let scan = MenuItem::with_id(&app, "scan", "立即扫描", true, None::<&str>)?;
    let low_power_item = CheckMenuItem::with_id(
        &app,
        "low-power",
        "低功耗模式",
        true,
        low_power,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(&app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(&app, &[&show, &scan, &low_power_item, &quit])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().expect("应用图标随包内置").clone())
        .tooltip(toktol_core::paths::PRODUCT_NAME)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            // 显示主窗口即退出低功耗：两者是同一状态的两面。
            "show" => exit_low_power(app),
            "scan" => {
                if let Err(err) = trigger_scan(app) {
                    eprintln!("触发扫描失败: {err}");
                }
            }
            "low-power" => {
                if app.state::<ShellConfigState>().get().low_power {
                    exit_low_power(app);
                } else {
                    enter_low_power(app);
                }
            }
            // 退出先停扫描循环（不 join，进程随即退出，轮内半截事务由 WAL 兜底）。
            "quit" => {
                if let Some(handle) = app
                    .state::<ScanLoopSlot>()
                    .lock()
                    .expect("扫描循环槽位锁中毒")
                    .take()
                {
                    handle.stop_soon();
                }
                app.exit(0);
            }
            _ => {}
        })
        .build(&app)?;
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_visible(visible)?;
    }
    app.manage(TrayMenu {
        show,
        scan,
        low_power: low_power_item,
        quit,
    });
    Ok(())
}

/// 低功耗勾选态的唯一写入口：托盘菜单、窗口事件与命令三条路都经它，勾选框与
/// ShellConfigState 永不漂移。
fn sync_low_power_item(manager: &AppHandle, low_power: bool) {
    if let Some(items) = manager.try_state::<TrayMenu>() {
        let _ = items.low_power.set_checked(low_power);
    }
}

/// 供命令模块转调的托盘操作；托盘内部状态（菜单句柄等）不出本模块。
pub(crate) mod tray_ops {
    use super::{TRAY_ID, TrayMenu, exit_low_power};
    use tauri::Manager;

    /// 托盘显隐切换；图标句柄此刻必然已建好（setup 里无条件创建）。
    /// 关托盘时若在低功耗（无窗状态会失去唯一恢复路径），先重建窗口再降级。
    pub fn set_enabled(app: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
        if !enabled {
            exit_low_power(app);
        }
        let tray = app
            .tray_by_id(TRAY_ID)
            .ok_or_else(|| "托盘尚未初始化".to_string())?;
        tray.set_visible(enabled).map_err(|e| e.to_string())
    }

    /// 菜单文案跟随界面语言；失败不致命（菜单停留在上一语言）。
    pub fn set_texts(app: &tauri::AppHandle, show: &str, scan: &str, low_power: &str, quit: &str) {
        let Some(items) = app.try_state::<TrayMenu>() else {
            return;
        };
        let _ = items.show.set_text(show);
        let _ = items.scan.set_text(scan);
        let _ = items.low_power.set_text(low_power);
        let _ = items.quit.set_text(quit);
    }
}
