// Em release no Windows o app roda sem janela de console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod gamemode;
mod monitor;
#[cfg(windows)]
mod tray;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, PhysicalPosition, SharedString, VecModel};

use config::Config;
use gamemode::GameDetector;
use monitor::{Monitor, Severity, Snapshot};

slint::include_modules!();

/// Mensagens da interface para a thread de monitoramento.
pub enum Command {
    ToggleGameMode,
    Quit,
}

/// Estado do modo jogo visto pela interface.
#[derive(Clone, Default)]
struct GameState {
    active: bool,
    game: Option<String>,
    forced: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cfg = Config::load_or_create().unwrap_or_else(|e| {
        log::error!("{e:#}; usando configuração padrão");
        Config::default()
    });
    let cfg = Arc::new(Mutex::new(cfg));

    let main_window = MainWindow::new()?;
    let widget = WidgetWindow::new()?;

    // Fechar a janela principal só a esconde; a Sexta-Feira continua na bandeja.
    main_window.window().on_close_requested(|| slint::CloseRequestResponse::HideWindow);
    widget.window().on_close_requested(|| slint::CloseRequestResponse::HideWindow);

    let (tx, rx) = mpsc::channel::<Command>();

    wire_main_window(&main_window, &widget, &tx);
    wire_widget(&widget, &main_window, &cfg);

    #[cfg(windows)]
    let _tray = tray::Tray::new(&main_window, &widget, tx.clone())?;

    {
        let c = cfg.lock().unwrap();
        widget.window().set_position(PhysicalPosition::new(c.widget.x, c.widget.y));
        if c.widget.enabled {
            widget.show()?;
        }
        main_window.set_widget_visible(c.widget.enabled);
    }
    main_window.show()?;

    let worker = spawn_monitor(cfg.lock().unwrap().clone(), rx, main_window.as_weak(), widget.as_weak());

    slint::run_event_loop_until_quit()?;

    let _ = tx.send(Command::Quit);
    let _ = worker.join();
    if let Err(e) = cfg.lock().unwrap().save() {
        log::warn!("não consegui salvar a configuração: {e:#}");
    }
    Ok(())
}

fn wire_main_window(main_window: &MainWindow, widget: &WidgetWindow, tx: &mpsc::Sender<Command>) {
    let (mw, wg) = (main_window.as_weak(), widget.as_weak());
    main_window.on_toggle_widget(move || {
        let (Some(mw), Some(wg)) = (mw.upgrade(), wg.upgrade()) else { return };
        toggle_widget(&mw, &wg);
    });

    let tx = tx.clone();
    main_window.on_toggle_game_mode(move || {
        let _ = tx.send(Command::ToggleGameMode);
    });

    main_window.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
}

pub fn toggle_widget(mw: &MainWindow, wg: &WidgetWindow) {
    let show = !mw.get_widget_visible();
    let _ = if show { wg.show() } else { wg.hide() };
    mw.set_widget_visible(show);
}

fn wire_widget(widget: &WidgetWindow, main_window: &MainWindow, cfg: &Arc<Mutex<Config>>) {
    let wg = widget.as_weak();
    widget.on_drag(move |dx, dy| {
        let Some(wg) = wg.upgrade() else { return };
        let w = wg.window();
        let scale = w.scale_factor();
        let pos = w.position();
        w.set_position(PhysicalPosition::new(pos.x + (dx * scale) as i32, pos.y + (dy * scale) as i32));
    });

    let (wg, cfg) = (widget.as_weak(), cfg.clone());
    widget.on_drag_finished(move || {
        let Some(wg) = wg.upgrade() else { return };
        let pos = wg.window().position();
        let mut c = cfg.lock().unwrap();
        c.widget.x = pos.x;
        c.widget.y = pos.y;
    });

    let mw = main_window.as_weak();
    widget.on_open_main(move || {
        if let Some(mw) = mw.upgrade() {
            let _ = mw.show();
        }
    });
}

fn spawn_monitor(
    cfg: Config,
    rx: mpsc::Receiver<Command>,
    mw: slint::Weak<MainWindow>,
    wg: slint::Weak<WidgetWindow>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("monitor".into())
        .spawn(move || {
            let mut monitor = Monitor::new(cfg.monitor.clone());
            let detector = GameDetector::new(cfg.game_mode.clone());
            let hide_widget_in_game = cfg.game_mode.hide_widget;
            let mut state = GameState::default();

            loop {
                let detected = detector.detect();
                let active = state.forced || detected.is_some();
                if active != state.active {
                    log::info!("modo jogo {}", if active { "ligado" } else { "desligado" });
                    gamemode::set_low_power(active);
                }
                state.active = active;
                state.game = detected;

                let snap = monitor.sample(state.active);
                push_to_ui(&mw, &wg, snap, state.clone(), hide_widget_in_game);

                match rx.recv_timeout(monitor.interval(state.active)) {
                    Ok(Command::ToggleGameMode) => state.forced = !state.forced,
                    Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        })
        .expect("falha ao iniciar a thread de monitoramento")
}

fn push_to_ui(
    mw: &slint::Weak<MainWindow>,
    wg: &slint::Weak<WidgetWindow>,
    s: Snapshot,
    game: GameState,
    hide_widget_in_game: bool,
) {
    let status = match (&game.game, game.active) {
        (Some(g), _) => format!("Modo jogo: {g}"),
        (None, true) => "Modo jogo (manual)".to_string(),
        (None, false) => "Online e monitorando".to_string(),
    };
    let cpu_temp = s.cpu_temp_c.map(|t| format!(" · {t:.0} °C")).unwrap_or_default();
    let cpu_detail = format!("Uso total{cpu_temp}");
    let ram_value = format!("{:.1} GB", s.ram_used_mb as f32 / 1024.0);
    let ram_detail = format!("de {:.1} GB ({:.0}%)", s.ram_total_mb as f32 / 1024.0, s.ram_pct());
    let (gpu_pct, gpu_value, gpu_detail, gpu_short) = match &s.gpu {
        Some(g) => (
            g.usage_pct,
            format!("{:.0}%", g.usage_pct),
            format!("{} · {:.0} °C · {}/{} MB", g.name, g.temp_c, g.vram_used_mb, g.vram_total_mb),
            format!("{:.0}% · {:.0}°C", g.usage_pct, g.temp_c),
        ),
        None => (0.0, "—".into(), "Sem GPU NVIDIA detectada".into(), "—".into()),
    };
    let self_detail = format!("Usando {} MB de RAM", s.self_mem_mb);

    let alerts: Vec<AlertRow> = s
        .alerts
        .iter()
        .map(|a| AlertRow {
            severity: match a.severity {
                Severity::Warning => 1,
                Severity::Critical => 2,
            },
            message: a.message.clone().into(),
        })
        .collect();
    let processes: Vec<ProcessRow> = s
        .top_processes
        .iter()
        .map(|p| ProcessRow {
            name: p.name.clone().into(),
            cpu: format!("{:.0}%", p.cpu_pct).into(),
            mem: format!("{} MB", p.mem_mb).into(),
        })
        .collect();
    let alert_count = alerts.len() as i32;
    let cpu_short = format!("{:.0}%{}", s.cpu_pct, s.cpu_temp_c.map(|t| format!(" · {t:.0}°C")).unwrap_or_default());
    let ram_short = format!("{:.0}%", s.ram_pct());
    let ram_pct = s.ram_pct();

    let status_mw = status.clone();
    let _ = mw.upgrade_in_event_loop(move |w| {
        w.set_status(status_mw.into());
        w.set_game_mode(game.active);
        w.set_cpu_pct(s.cpu_pct);
        w.set_cpu_detail(cpu_detail.into());
        w.set_ram_pct(ram_pct);
        w.set_ram_value(ram_value.into());
        w.set_ram_detail(ram_detail.into());
        w.set_gpu_pct(gpu_pct);
        w.set_gpu_value(gpu_value.into());
        w.set_gpu_detail(gpu_detail.into());
        w.set_self_detail(self_detail.into());
        w.set_alerts(ModelRc::new(VecModel::from(alerts)));
        w.set_processes(ModelRc::new(VecModel::from(processes)));
    });

    let mw2 = mw.clone();
    let _ = wg.upgrade_in_event_loop(move |w| {
        w.set_status(SharedString::from(if game.active { "modo jogo" } else { "online" }));
        w.set_game_mode(game.active);
        w.set_cpu_pct(s.cpu_pct);
        w.set_ram_pct(ram_pct);
        w.set_gpu_pct(gpu_pct);
        w.set_cpu_text(cpu_short.into());
        w.set_ram_text(ram_short.into());
        w.set_gpu_text(gpu_short.into());
        w.set_alert_count(alert_count);
        if hide_widget_in_game {
            let wanted = mw2.upgrade().is_some_and(|m| m.get_widget_visible());
            let _ = if game.active || !wanted { w.hide() } else { w.show() };
        }
    });
}
