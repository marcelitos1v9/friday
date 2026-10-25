//! Ações no sistema: abrir apps, volume, mídia e fechar programas.
//!
//! Tudo aqui é chamado sob demanda pela thread do cérebro; nada fica rodando em segundo plano.

pub mod apps;

use anyhow::{bail, Result};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeAction {
    Set(u8),
    Up(u8),
    Down(u8),
    Mute,
    Unmute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKey {
    PlayPause,
    Next,
    Previous,
    Stop,
}

/// Abre um caminho, atalho ou URL pelo shell do Windows.
pub fn launch(target: &str) -> Result<()> {
    platform::launch(target)
}

/// Aplica a ação de volume e devolve o volume final (0–100) e se está mudo.
pub fn volume(action: VolumeAction) -> Result<(u8, bool)> {
    platform::volume(action)
}

pub fn media(key: MediaKey) -> Result<()> {
    platform::media(key)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMatch {
    pub name: String,
    pub pids: Vec<u32>,
}

fn process_system() -> System {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    sys
}

fn strip_exe(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").map(str::to_string).unwrap_or(lower)
}

/// Processos cujo nome combina com o pedido ("chrome" acha "chrome.exe"), agrupados por nome.
pub fn find_processes(query: &str) -> Vec<ProcessMatch> {
    let q = strip_exe(query.trim());
    if q.is_empty() {
        return Vec::new();
    }
    let sys = process_system();
    let mut groups: Vec<ProcessMatch> = Vec::new();
    for (pid, p) in sys.processes() {
        if p.thread_kind().is_some() {
            continue;
        }
        let name = p.name().to_string_lossy().into_owned();
        let base = strip_exe(&name);
        if base != q && !(q.len() >= 4 && base.starts_with(&q)) {
            continue;
        }
        match groups.iter_mut().find(|g| g.name == name) {
            Some(g) => g.pids.push(pid.as_u32()),
            None => groups.push(ProcessMatch { name, pids: vec![pid.as_u32()] }),
        }
    }
    // Nunca deixa a Sexta-Feira se fechar sozinha nem mexer em processos críticos do Windows.
    let me = std::process::id();
    groups.retain(|g| !g.pids.contains(&me) && !is_protected(&g.name));
    groups
}

fn is_protected(name: &str) -> bool {
    const PROTECTED: &[&str] = &[
        "system", "csrss", "wininit", "winlogon", "services", "lsass", "smss", "svchost", "dwm", "explorer",
        "fontdrvhost", "sexta-feira",
    ];
    PROTECTED.contains(&strip_exe(name).as_str())
}

/// Programas com janela que o Windows marca como "Não está respondendo".
pub fn hung_programs() -> Vec<ProcessMatch> {
    let pids = platform::hung_window_pids();
    if pids.is_empty() {
        return Vec::new();
    }
    let sys = process_system();
    let mut groups: Vec<ProcessMatch> = Vec::new();
    for pid in pids {
        let Some(p) = sys.process(sysinfo::Pid::from_u32(pid)) else { continue };
        let name = p.name().to_string_lossy().into_owned();
        match groups.iter_mut().find(|g| g.name == name) {
            Some(g) if !g.pids.contains(&pid) => g.pids.push(pid),
            Some(_) => {}
            None => groups.push(ProcessMatch { name, pids: vec![pid] }),
        }
    }
    groups.retain(|g| !is_protected(&g.name));
    groups
}

/// Fecha os processos. Sem `force`, pede para as janelas fecharem (o app pode salvar);
/// com `force`, encerra na hora (necessário para programa travado).
pub fn close(target: &ProcessMatch, force: bool) -> Result<usize> {
    if is_protected(&target.name) {
        bail!("{} é um processo do sistema e não pode ser fechado", target.name);
    }
    if !force {
        let n = platform::close_windows(&target.pids);
        if n > 0 {
            return Ok(n);
        }
        // Sem janela visível (ex.: só na bandeja): cai para o encerramento direto.
    }
    let sys = process_system();
    let mut killed = 0;
    for pid in &target.pids {
        if let Some(p) = sys.process(sysinfo::Pid::from_u32(*pid)) {
            if p.kill() {
                killed += 1;
            }
        }
    }
    if killed == 0 {
        bail!("não consegui encerrar {} (talvez precise de administrador)", target.name);
    }
    Ok(killed)
}

#[cfg(windows)]
mod platform {
    use super::{MediaKey, VolumeAction};
    use anyhow::{anyhow, Result};
    use windows::core::{BOOL, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, PostMessageW, SW_SHOWNORMAL,
        WM_CLOSE,
    };

    pub fn launch(target: &str) -> Result<()> {
        let file = HSTRING::from(target);
        let r = unsafe { ShellExecuteW(None, &HSTRING::from("open"), &file, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
        // Pela documentação, valores <= 32 são códigos de erro.
        if r.0 as isize <= 32 {
            return Err(anyhow!("o Windows não conseguiu abrir {target} (código {})", r.0 as isize));
        }
        Ok(())
    }

    fn endpoint() -> Result<IAudioEndpointVolume> {
        unsafe {
            // Pode já estar inicializado nesta thread; tanto faz.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            Ok(dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)?)
        }
    }

    pub fn volume(action: VolumeAction) -> Result<(u8, bool)> {
        let ep = endpoint()?;
        unsafe {
            let cur = (ep.GetMasterVolumeLevelScalar()? * 100.0).round() as i32;
            let set = |v: i32| -> Result<()> {
                ep.SetMasterVolumeLevelScalar(v.clamp(0, 100) as f32 / 100.0, std::ptr::null())?;
                ep.SetMute(false, std::ptr::null())?;
                Ok(())
            };
            match action {
                VolumeAction::Set(v) => set(v as i32)?,
                VolumeAction::Up(d) => set(cur + d as i32)?,
                VolumeAction::Down(d) => set(cur - d as i32)?,
                VolumeAction::Mute => ep.SetMute(true, std::ptr::null())?,
                VolumeAction::Unmute => ep.SetMute(false, std::ptr::null())?,
            }
            let level = (ep.GetMasterVolumeLevelScalar()? * 100.0).round() as u8;
            let muted = ep.GetMute()?.as_bool();
            Ok((level, muted))
        }
    }

    pub fn media(key: MediaKey) -> Result<()> {
        let vk: VIRTUAL_KEY = match key {
            MediaKey::PlayPause => VK_MEDIA_PLAY_PAUSE,
            MediaKey::Next => VK_MEDIA_NEXT_TRACK,
            MediaKey::Previous => VK_MEDIA_PREV_TRACK,
            MediaKey::Stop => VK_MEDIA_STOP,
        };
        let input = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        let inputs = [input(KEYEVENTF_EXTENDEDKEY), input(KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP)];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            return Err(anyhow!("o Windows recusou a tecla de mídia"));
        }
        Ok(())
    }

    struct Scan {
        want: Vec<u32>,
        found: Vec<(HWND, u32)>,
        hung_only: bool,
    }

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let scan = unsafe { &mut *(lparam.0 as *mut Scan) };
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return true.into();
            }
            if scan.hung_only && !IsHungAppWindow(hwnd).as_bool() {
                return true.into();
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if scan.want.is_empty() || scan.want.contains(&pid) {
                scan.found.push((hwnd, pid));
            }
        }
        true.into()
    }

    fn windows_of(want: &[u32], hung_only: bool) -> Vec<(HWND, u32)> {
        let mut scan = Scan { want: want.to_vec(), found: Vec::new(), hung_only };
        unsafe {
            let _ = EnumWindows(Some(enum_cb), LPARAM(&mut scan as *mut Scan as isize));
        }
        scan.found
    }

    pub fn hung_window_pids() -> Vec<u32> {
        let mut pids: Vec<u32> = windows_of(&[], true).into_iter().map(|(_, p)| p).collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    pub fn close_windows(pids: &[u32]) -> usize {
        let wins = windows_of(pids, false);
        let mut n = 0;
        for (hwnd, _) in wins {
            if unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok() {
                n += 1;
            }
        }
        n
    }
}

#[cfg(not(windows))]
mod platform {
    //! Fora do Windows (desenvolvimento) as ações só existem o bastante para testar o fluxo.
    use super::{MediaKey, VolumeAction};
    use anyhow::{bail, Result};

    pub fn launch(target: &str) -> Result<()> {
        std::process::Command::new("xdg-open").arg(target).spawn()?;
        Ok(())
    }

    pub fn volume(_action: VolumeAction) -> Result<(u8, bool)> {
        bail!("controle de volume só no Windows")
    }

    pub fn media(_key: MediaKey) -> Result<()> {
        bail!("teclas de mídia só no Windows")
    }

    pub fn hung_window_pids() -> Vec<u32> {
        Vec::new()
    }

    pub fn close_windows(_pids: &[u32]) -> usize {
        0
    }
}
