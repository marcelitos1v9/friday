//! Detecção de jogo e economia de recursos enquanto você joga.
//!
//! Um jogo é reconhecido quando o executável em primeiro plano está na lista da configuração
//! ou quando a janela em primeiro plano ocupa o monitor inteiro. Ao entrar no modo jogo a
//! Sexta-Feira baixa a própria prioridade e liga o EcoQoS do Windows, que manda o sistema
//! rodar o processo nos núcleos de eficiência e em frequência baixa.

use crate::config::GameModeConfig;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Foreground {
    pub exe: String,
    pub fullscreen: bool,
}

pub struct GameDetector {
    cfg: GameModeConfig,
    exes: Vec<String>,
}

impl GameDetector {
    pub fn new(cfg: GameModeConfig) -> Self {
        let exes = cfg.executables.iter().map(|e| e.to_lowercase()).collect();
        Self { cfg, exes }
    }

    /// Retorna o nome do jogo detectado, se houver.
    pub fn detect(&self) -> Option<String> {
        if !self.cfg.auto {
            return None;
        }
        let fg = platform::foreground()?;
        let exe = fg.exe.to_lowercase();
        if self.exes.iter().any(|e| *e == exe) || (self.cfg.detect_fullscreen && fg.fullscreen) {
            Some(fg.exe)
        } else {
            None
        }
    }
}

/// Liga ou desliga o modo de baixo consumo do próprio processo.
pub fn set_low_power(on: bool) {
    platform::set_low_power(on);
}

#[cfg(windows)]
mod platform {
    use super::Foreground;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, ProcessPowerThrottling, QueryFullProcessImageNameW,
        SetPriorityClass, SetProcessInformation, BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
        PROCESS_NAME_WIN32, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowRect, GetWindowThreadProcessId,
    };

    pub fn foreground() -> Option<Foreground> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_invalid() || hwnd == GetDesktopWindow() || hwnd == GetShellWindow() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 || pid == GetCurrentProcessId() {
                return None;
            }
            let exe = exe_name(pid)?;
            // Explorer em tela cheia (área de trabalho) não é jogo.
            if exe.eq_ignore_ascii_case("explorer.exe") {
                return None;
            }
            Some(Foreground { fullscreen: is_fullscreen(hwnd), exe })
        }
    }

    unsafe fn exe_name(pid: u32) -> Option<String> {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_string)
    }

    unsafe fn is_fullscreen(hwnd: HWND) -> bool {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
    }

    pub fn set_low_power(on: bool) {
        unsafe {
            let process = GetCurrentProcess();
            let class = if on { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS };
            if let Err(e) = SetPriorityClass(process, class) {
                log::warn!("não consegui mudar a prioridade: {e}");
            }
            let state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
                StateMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
            };
            if let Err(e) = SetProcessInformation(
                process,
                ProcessPowerThrottling,
                &state as *const _ as *const _,
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            ) {
                log::warn!("não consegui ligar o EcoQoS: {e}");
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Foreground;

    /// Fora do Windows (desenvolvimento) não há detecção de primeiro plano.
    pub fn foreground() -> Option<Foreground> {
        None
    }

    pub fn set_low_power(on: bool) {
        log::debug!("modo de baixo consumo: {on} (sem efeito fora do Windows)");
    }
}
