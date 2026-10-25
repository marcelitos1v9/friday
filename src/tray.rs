//! Ícone na bandeja do Windows e atalho global (Ctrl+Alt+F abre a Sexta-Feira).
//!
//! Os eventos chegam por callbacks, sem nenhum timer de polling.

use std::sync::mpsc::Sender;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use slint::ComponentHandle;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::{toggle_widget, Command, MainWindow, WidgetWindow};

pub struct Tray {
    _icon: TrayIcon,
    _hotkeys: Option<GlobalHotKeyManager>,
}

impl Tray {
    pub fn new(main_window: &MainWindow, widget: &WidgetWindow, tx: Sender<Command>) -> anyhow::Result<Self> {
        let open = MenuItem::new("Abrir Sexta-Feira", true, None);
        let toggle_w = MenuItem::new("Mostrar/ocultar widget", true, None);
        let game = MenuItem::new("Alternar modo jogo", true, None);
        let quit = MenuItem::new("Sair", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &toggle_w, &game, &PredefinedMenuItem::separator(), &quit])?;

        let icon = TrayIconBuilder::new()
            .with_tooltip("Sexta-Feira")
            .with_icon(ring_icon(32)?)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;

        let (open_id, toggle_id, game_id, quit_id) =
            (open.id().clone(), toggle_w.id().clone(), game.id().clone(), quit.id().clone());
        let (mw, wg) = (main_window.as_weak(), widget.as_weak());
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            let id = ev.id;
            if id == game_id {
                let _ = tx.send(Command::ToggleGameMode);
                return;
            }
            let (mw, wg) = (mw.clone(), wg.clone());
            let (open_id, toggle_id, quit_id) = (open_id.clone(), toggle_id.clone(), quit_id.clone());
            let _ = slint::invoke_from_event_loop(move || {
                if id == quit_id {
                    let _ = slint::quit_event_loop();
                } else if id == open_id {
                    if let Some(m) = mw.upgrade() {
                        let _ = m.show();
                    }
                } else if id == toggle_id {
                    if let (Some(m), Some(w)) = (mw.upgrade(), wg.upgrade()) {
                        toggle_widget(&m, &w);
                    }
                }
            });
        }));

        // Clique esquerdo no ícone abre a janela principal.
        let mw = main_window.as_weak();
        TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, .. } = ev {
                let mw = mw.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(m) = mw.upgrade() {
                        let _ = m.show();
                    }
                });
            }
        }));

        let hotkeys = register_hotkey(main_window);
        Ok(Self { _icon: icon, _hotkeys: hotkeys })
    }
}

fn register_hotkey(main_window: &MainWindow) -> Option<GlobalHotKeyManager> {
    let manager = GlobalHotKeyManager::new().ok()?;
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyF);
    if let Err(e) = manager.register(hotkey) {
        log::warn!("atalho Ctrl+Alt+F indisponível: {e}");
        return None;
    }
    let mw = main_window.as_weak();
    GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
        if ev.state != HotKeyState::Pressed {
            return;
        }
        let mw = mw.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(m) = mw.upgrade() {
                if m.window().is_visible() {
                    let _ = m.hide();
                } else {
                    let _ = m.show();
                }
            }
        });
    }));
    Some(manager)
}

/// Desenha o ícone (anel ciano com núcleo) direto em RGBA, sem precisar de decodificador de imagem.
fn ring_icon(size: u32) -> anyhow::Result<Icon> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let c = (size as f32 - 1.0) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt() / c;
            let px = if d <= 0.25 || (0.78..=1.0).contains(&d) || (0.48..=0.58).contains(&d) {
                [58, 215, 255, 255]
            } else if d <= 1.0 {
                [5, 8, 14, 255]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&px);
        }
    }
    Ok(Icon::from_rgba(rgba, size, size)?)
}
