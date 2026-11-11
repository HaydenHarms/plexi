//! System integration that runs outside the window: global hotkeys and the tray icon.
//! Both report through a channel and wake the UI with a repaint request, so they work
//! while the overlay is click-through and egui receives no input.

use std::sync::mpsc::{Receiver, Sender, channel};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellEvent {
    ToggleDraw,
    /// Start drawing on the screen (from a later `plexi --draw`).
    Draw,
    /// Open a notebook (from a later launch with a file).
    Open(std::path::PathBuf),
    ClearOverlay,
    ShowNotebook,
    Snip,
    Quit,
}

pub struct Shell {
    pub events: Receiver<ShellEvent>,
    _tray: Option<TrayIcon>,
    _hotkeys: Option<GlobalHotKeyManager>,
    instance: Option<crate::instance::Listening>,
    /// Hotkeys that could not be registered (already taken by another app).
    pub failed_hotkeys: Vec<String>,
}

fn parse_hotkey(s: &str) -> Option<HotKey> {
    // global-hotkey wants names like "control+alt+KeyD".
    let normalized: Vec<String> = s
        .split('+')
        .map(|p| p.trim())
        .map(|p| match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "control".to_owned(),
            "alt" => "alt".to_owned(),
            "shift" => "shift".to_owned(),
            "win" | "super" | "meta" => "super".to_owned(),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphabetic()) => format!("Key{}", k.to_ascii_uppercase()),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()) => format!("Digit{k}"),
            _ => p.to_owned(),
        })
        .collect();
    normalized.join("+").parse().ok()
}

impl Shell {
    pub fn start(ctx: &egui::Context, cfg: &crate::config::Config, instance: Option<crate::instance::Primary>) -> Self {
        let (tx, rx) = channel();
        let instance = instance.map(|p| {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            p.serve(move |req| {
                use crate::instance::Request;
                let ev = match req {
                    Request::Show => ShellEvent::ShowNotebook,
                    Request::Draw => ShellEvent::Draw,
                    Request::Open(p) => ShellEvent::Open(p),
                    Request::Nothing => return,
                };
                let _ = tx.send(ev);
                ctx.request_repaint();
            })
        });
        let mut failed = Vec::new();

        let hotkeys = GlobalHotKeyManager::new().ok().inspect(|mgr| {
            let mut bound: Vec<(u32, ShellEvent)> = Vec::new();
            for (text, ev) in [
                (&cfg.hotkey_draw, ShellEvent::ToggleDraw),
                (&cfg.hotkey_clear, ShellEvent::ClearOverlay),
                (&cfg.hotkey_notebook, ShellEvent::ShowNotebook),
                (&cfg.hotkey_snip, ShellEvent::Snip),
            ] {
                match parse_hotkey(text) {
                    Some(hk) if mgr.register(hk).is_ok() => bound.push((hk.id(), ev)),
                    _ => failed.push(text.clone()),
                }
            }
            let (tx, ctx) = (tx.clone(), ctx.clone());
            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                if e.state == HotKeyState::Pressed
                    && let Some((_, ev)) = bound.iter().find(|(id, _)| *id == e.id)
                {
                    let _ = tx.send(ev.clone());
                    ctx.request_repaint();
                }
            }));
        });

        let tray = build_tray(ctx, tx.clone(), cfg);
        Self { events: rx, _tray: tray, _hotkeys: hotkeys, instance, failed_hotkeys: failed }
    }

    /// Plexi is quitting: later launches should wait for it to go, not hand over to it.
    pub fn closing(&self) {
        if let Some(l) = &self.instance {
            l.stop();
        }
    }
}

fn build_tray(ctx: &egui::Context, tx: Sender<ShellEvent>, cfg: &crate::config::Config) -> Option<TrayIcon> {
    let draw = MenuItem::with_id("draw", format!("Draw on screen\t{}", cfg.hotkey_draw), true, None);
    let clear = MenuItem::with_id("clear", format!("Clear screen ink\t{}", cfg.hotkey_clear), true, None);
    let notebook = MenuItem::with_id("notebook", format!("Open notebook\t{}", cfg.hotkey_notebook), true, None);
    let snip = MenuItem::with_id("snip", format!("Snip an area\t{}", cfg.hotkey_snip), true, None);
    let quit = MenuItem::with_id("quit", "Quit Plexi", true, None);
    let menu = Menu::new();
    menu.append_items(&[&draw, &snip, &clear, &notebook, &PredefinedMenuItem::separator(), &quit]).ok()?;

    {
        let (tx, ctx) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let ev = match e.id.as_ref() {
                "draw" => ShellEvent::ToggleDraw,
                "clear" => ShellEvent::ClearOverlay,
                "notebook" => ShellEvent::ShowNotebook,
                "snip" => ShellEvent::Snip,
                "quit" => ShellEvent::Quit,
                _ => return,
            };
            let _ = tx.send(ev);
            ctx.request_repaint();
        }));
    }
    {
        let ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = tx.send(ShellEvent::ToggleDraw);
                ctx.request_repaint();
            }
        }));
    }

    let icon = crate::app_icon_rgba(32);
    let icon = tray_icon::Icon::from_rgba(icon.0, icon.1, icon.2).ok()?;
    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(format!("Plexi — {} to draw on screen", cfg.hotkey_draw))
        .with_icon(icon)
        .build()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_strings_parse() {
        assert!(parse_hotkey("Ctrl+Alt+D").is_some());
        assert!(parse_hotkey("ctrl+shift+1").is_some());
        assert!(parse_hotkey("Ctrl+Alt+F9").is_some());
        assert!(parse_hotkey("nonsense+").is_none());
    }
}
