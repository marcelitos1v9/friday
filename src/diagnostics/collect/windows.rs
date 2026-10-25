//! Coletores do Windows: Log de Eventos (wevtapi) e WMI (discos, drivers, dispositivos e
//! sensores). Algumas consultas exigem administrador; sem permissão elas viram uma nota.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS};
use windows::Win32::System::EventLog::{
    EvtClose, EvtCreateRenderContext, EvtFormatMessage, EvtFormatMessageEvent, EvtNext, EvtOpenPublisherMetadata,
    EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection, EvtRender, EvtRenderContextSystem, EvtRenderEventValues,
    EvtSystemEventID, EvtSystemLevel, EvtSystemProviderName, EvtSystemTimeCreated, EvtVarTypeByte, EvtVarTypeFileTime,
    EvtVarTypeString, EvtVarTypeUInt16, EVT_HANDLE, EVT_VARIANT,
};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
use wmi::WMIConnection;

use super::{DiskHealth, DriverInfo, EventRecord, ProblemDevice, RawData};
use crate::config::DiagnosticsConfig;
use crate::diagnostics::{rules, SensorReading};

const NEED_ADMIN: &str = "execute a Sexta-Feira como administrador para ver este item";

pub fn enter_background_mode() {
    // Prioridade de segundo plano de verdade: CPU, disco e memória com prioridade baixa.
    unsafe {
        if let Err(e) = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) {
            log::warn!("não consegui baixar a prioridade do diagnóstico: {e}");
        }
    }
}

pub fn collect(cfg: &DiagnosticsConfig, raw: &mut RawData) {
    raw.events = read_events(cfg.event_days);

    match WMIConnection::new() {
        Ok(cimv2) => {
            raw.drivers = drivers(&cimv2);
            raw.problem_devices = problem_devices(&cimv2);
        }
        Err(e) => {
            raw.drivers = Err(format!("WMI indisponível: {e}"));
            raw.problem_devices = Err(format!("WMI indisponível: {e}"));
        }
    }

    raw.disks = disks(&mut raw.notes);
    hardware_monitor_sensors(raw);
}

// ---------------------------------------------------------------- Log de Eventos

/// Limite de eventos lidos por canal, para a análise nunca demorar demais.
const MAX_EVENTS_PER_CHANNEL: usize = 3000;
const MAX_MESSAGE_CHARS: usize = 400;

struct EvtHandle(EVT_HANDLE);

impl Drop for EvtHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = EvtClose(self.0);
            }
        }
    }
}

fn read_events(days: u32) -> Result<Vec<EventRecord>, String> {
    let ms = u64::from(days.max(1)) * 86_400_000;
    let query = format!("*[System[(Level=1 or Level=2) and TimeCreated[timediff(@SystemTime) <= {ms}]]]");
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for channel in ["System", "Application"] {
        if let Err(e) = unsafe { read_channel(channel, &query, &mut out) } {
            errors.push(format!("{channel}: {e}"));
        }
    }
    if out.is_empty() && !errors.is_empty() {
        Err(errors.join("; "))
    } else {
        Ok(out)
    }
}

unsafe fn read_channel(channel: &str, query: &str, out: &mut Vec<EventRecord>) -> windows::core::Result<()> {
    let channel_w = HSTRING::from(channel);
    let query_w = HSTRING::from(query);
    let results = EvtHandle(EvtQuery(
        None,
        PCWSTR(channel_w.as_ptr()),
        PCWSTR(query_w.as_ptr()),
        EvtQueryChannelPath.0 | EvtQueryReverseDirection.0,
    )?);
    let ctx = EvtHandle(EvtCreateRenderContext(None, EvtRenderContextSystem.0)?);

    let mut publishers: HashMap<String, Option<EvtHandle>> = HashMap::new();
    let mut formatted: HashSet<(String, u32)> = HashSet::new();
    let mut values: Vec<u64> = vec![0; 1024];
    let mut batch = [0isize; 64];
    let mut read = 0usize;

    while read < MAX_EVENTS_PER_CHANNEL {
        let mut returned = 0u32;
        if let Err(e) = EvtNext(results.0, &mut batch, 2000, 0, &mut returned) {
            if e.code() == ERROR_NO_MORE_ITEMS.to_hresult() {
                break;
            }
            return Err(e);
        }
        // Todos os handles do lote precisam ser fechados, mesmo os que não usarmos.
        let events: Vec<EvtHandle> = batch[..returned as usize].iter().map(|&h| EvtHandle(EVT_HANDLE(h))).collect();
        for ev in &events {
            if read >= MAX_EVENTS_PER_CHANNEL {
                break;
            }
            read += 1;
            let Some(mut rec) = render_system(&ctx, ev, &mut values) else { continue };
            // Formatar mensagem é caro: só a mais recente de cada tipo, salvo quando a regra
            // precisa de todas (por exemplo, para saber quais programas travaram).
            let key = (rec.provider.clone(), rec.event_id);
            if rules::needs_every_message(&rec.provider, rec.event_id) || formatted.insert(key) {
                let publisher = publishers.entry(rec.provider.clone()).or_insert_with(|| {
                    let name = HSTRING::from(rec.provider.as_str());
                    EvtOpenPublisherMetadata(None, PCWSTR(name.as_ptr()), PCWSTR::null(), 0, 0).ok().map(EvtHandle)
                });
                rec.message = format_message(publisher.as_ref().map(|p| p.0), ev.0).unwrap_or_default();
            }
            out.push(rec);
        }
        if returned == 0 {
            break;
        }
    }
    Ok(())
}

unsafe fn render_system(ctx: &EvtHandle, ev: &EvtHandle, values: &mut Vec<u64>) -> Option<EventRecord> {
    let mut used = 0u32;
    let mut count = 0u32;
    let mut size = (values.len() * 8) as u32;
    if EvtRender(Some(ctx.0), ev.0, EvtRenderEventValues.0, size, Some(values.as_mut_ptr().cast()), &mut used, &mut count)
        .is_err()
    {
        if used <= size {
            return None;
        }
        values.resize((used as usize).div_ceil(8), 0);
        size = (values.len() * 8) as u32;
        EvtRender(Some(ctx.0), ev.0, EvtRenderEventValues.0, size, Some(values.as_mut_ptr().cast()), &mut used, &mut count)
            .ok()?;
    }
    let vals = std::slice::from_raw_parts(values.as_ptr().cast::<EVT_VARIANT>(), count as usize);
    let get = |id: i32| vals.get(id as usize).filter(|v| v.Type & 0x7f != 0);

    let provider = get(EvtSystemProviderName.0)
        .filter(|v| v.Type & 0x7f == EvtVarTypeString.0 as u32)
        .and_then(|v| v.Anonymous.StringVal.to_string().ok())
        .unwrap_or_default();
    let event_id = get(EvtSystemEventID.0)
        .filter(|v| v.Type & 0x7f == EvtVarTypeUInt16.0 as u32)
        .map_or(0, |v| u32::from(v.Anonymous.UInt16Val));
    let level = get(EvtSystemLevel.0).filter(|v| v.Type & 0x7f == EvtVarTypeByte.0 as u32).map_or(0, |v| v.Anonymous.ByteVal);
    let time_unix = get(EvtSystemTimeCreated.0)
        .filter(|v| v.Type & 0x7f == EvtVarTypeFileTime.0 as u32)
        .map_or(0, |v| filetime_to_unix(v.Anonymous.FileTimeVal));

    Some(EventRecord { provider, event_id, level, time_unix, message: String::new() })
}

unsafe fn format_message(publisher: Option<EVT_HANDLE>, ev: EVT_HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 2048];
    let mut used = 0u32;
    let mut result = EvtFormatMessage(publisher, Some(ev), 0, None, EvtFormatMessageEvent.0, Some(&mut buf), &mut used);
    if let Err(e) = &result {
        if e.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() && used as usize > buf.len() {
            buf.resize(used as usize, 0);
            result = EvtFormatMessage(publisher, Some(ev), 0, None, EvtFormatMessageEvent.0, Some(&mut buf), &mut used);
        }
    }
    // Mesmo com erro (valor de inserção não resolvido) costuma vir um texto aproveitável.
    if result.is_err() && used == 0 {
        return None;
    }
    let len = buf.iter().take(used as usize).position(|&c| c == 0).unwrap_or((used as usize).min(buf.len()));
    let text = String::from_utf16_lossy(&buf[..len]);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(text.chars().take(MAX_MESSAGE_CHARS).collect())
}

fn filetime_to_unix(ft: u64) -> i64 {
    (ft as i64 - 116_444_736_000_000_000) / 10_000_000
}

// ---------------------------------------------------------------- Discos

#[derive(Deserialize)]
#[serde(rename = "MSFT_PhysicalDisk", rename_all = "PascalCase")]
struct PhysicalDisk {
    object_id: Option<String>,
    device_id: Option<String>,
    friendly_name: Option<String>,
    media_type: Option<u16>,
    health_status: Option<u16>,
}

#[derive(Deserialize)]
#[serde(rename = "MSFT_StorageReliabilityCounter", rename_all = "PascalCase")]
struct ReliabilityCounter {
    device_id: Option<String>,
    temperature: Option<u32>,
    temperature_max: Option<u32>,
    wear: Option<u32>,
    read_errors_uncorrected: Option<u64>,
    write_errors_uncorrected: Option<u64>,
    power_on_hours: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename = "MSStorageDriver_FailurePredictStatus", rename_all = "PascalCase")]
struct FailurePredict {
    instance_name: Option<String>,
    predict_failure: Option<bool>,
}

const COUNTER_FIELDS: &str =
    "DeviceId, Temperature, TemperatureMax, Wear, ReadErrorsUncorrected, WriteErrorsUncorrected, PowerOnHours";

fn disks(notes: &mut Vec<String>) -> Result<Vec<DiskHealth>, String> {
    let storage = WMIConnection::with_namespace_path("ROOT\\Microsoft\\Windows\\Storage")
        .map_err(|e| format!("WMI de armazenamento indisponível: {e}"))?;
    let physical: Vec<PhysicalDisk> = storage
        .raw_query("SELECT ObjectId, DeviceId, FriendlyName, MediaType, HealthStatus FROM MSFT_PhysicalDisk")
        .map_err(|e| format!("não consegui listar os discos: {e}"))?;

    // Os contadores (temperatura, desgaste, erros) normalmente exigem administrador.
    let mut counters: Vec<ReliabilityCounter> =
        storage.raw_query(format!("SELECT {COUNTER_FIELDS} FROM MSFT_StorageReliabilityCounter")).unwrap_or_default();

    let mut out = Vec::new();
    for pd in &physical {
        let device_id = pd.device_id.clone().unwrap_or_default();
        let mut counter = counters.iter().position(|c| c.device_id.as_deref() == Some(device_id.as_str())).map(|i| counters.swap_remove(i));
        if counter.is_none() {
            if let Some(object_id) = &pd.object_id {
                let escaped = object_id.replace('\\', "\\\\").replace('"', "\\\"");
                let q = format!(
                    "ASSOCIATORS OF {{MSFT_PhysicalDisk.ObjectId=\"{escaped}\"}} \
                     WHERE AssocClass = MSFT_PhysicalDiskToStorageReliabilityCounter"
                );
                counter = storage.raw_query::<ReliabilityCounter>(q).ok().and_then(|mut v| v.pop());
            }
        }
        let c = counter.as_ref();
        out.push(DiskHealth {
            name: pd.friendly_name.clone().unwrap_or_else(|| format!("Disco {device_id}")),
            media: match pd.media_type {
                Some(3) => "HDD".into(),
                Some(4) => "SSD".into(),
                _ => String::new(),
            },
            health: pd.health_status,
            predict_failure: None,
            temp_c: c.and_then(|c| c.temperature).filter(|t| *t > 0).map(|t| t as f32),
            temp_max_c: c.and_then(|c| c.temperature_max).filter(|t| *t > 0).map(|t| t as f32),
            wear_pct: c.and_then(|c| c.wear),
            read_errors_uncorrected: c.and_then(|c| c.read_errors_uncorrected),
            write_errors_uncorrected: c.and_then(|c| c.write_errors_uncorrected),
            power_on_hours: c.and_then(|c| c.power_on_hours),
        });
    }
    if out.iter().all(|d| d.wear_pct.is_none() && d.power_on_hours.is_none()) && !out.is_empty() {
        notes.push(format!("Desgaste, horas de uso e erros dos discos: {NEED_ADMIN}."));
    }

    // Previsão de falha do SMART clássico (também exige administrador).
    let smart: Vec<FailurePredict> = WMIConnection::with_namespace_path("ROOT\\WMI")
        .and_then(|c| c.raw_query("SELECT InstanceName, PredictFailure FROM MSStorageDriver_FailurePredictStatus"))
        .unwrap_or_default();
    for s in smart {
        let instance = s.instance_name.unwrap_or_default();
        let fails = s.predict_failure.unwrap_or(false);
        let key = normalize(&instance);
        match out.iter_mut().find(|d| {
            let n = normalize(&d.name);
            !n.is_empty() && key.contains(&n)
        }) {
            Some(d) => d.predict_failure = Some(fails || d.predict_failure.unwrap_or(false)),
            None if fails => out.push(DiskHealth {
                name: rules::pretty_instance_name(&instance),
                predict_failure: Some(true),
                ..Default::default()
            }),
            None => {}
        }
    }
    Ok(out)
}

/// Só letras e números, minúsculos: "Samsung SSD 970" -> "samsungssd970".
fn normalize(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

// ---------------------------------------------------------------- Drivers e dispositivos

#[derive(Deserialize)]
#[serde(rename = "Win32_PnPSignedDriver", rename_all = "PascalCase")]
struct SignedDriver {
    device_name: Option<String>,
    device_class: Option<String>,
    driver_provider_name: Option<String>,
    driver_version: Option<String>,
    driver_date: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename = "Win32_PnPEntity", rename_all = "PascalCase")]
struct PnpEntity {
    name: Option<String>,
    #[serde(rename = "PNPClass")]
    pnp_class: Option<String>,
    config_manager_error_code: Option<u32>,
}

fn drivers(wmi: &WMIConnection) -> Result<Vec<DriverInfo>, String> {
    let list: Vec<SignedDriver> = wmi
        .raw_query(
            "SELECT DeviceName, DeviceClass, DriverProviderName, DriverVersion, DriverDate FROM Win32_PnPSignedDriver",
        )
        .map_err(|e| format!("não consegui listar os drivers: {e}"))?;
    Ok(list
        .into_iter()
        .filter_map(|d| {
            Some(DriverInfo {
                device: d.device_name?,
                class: d.device_class.unwrap_or_default(),
                provider: d.driver_provider_name.unwrap_or_default(),
                version: d.driver_version.unwrap_or_default(),
                date: d.driver_date.as_deref().and_then(rules::parse_wmi_date),
            })
        })
        .collect())
}

fn problem_devices(wmi: &WMIConnection) -> Result<Vec<ProblemDevice>, String> {
    let list: Vec<PnpEntity> = wmi
        .raw_query("SELECT Name, PNPClass, ConfigManagerErrorCode FROM Win32_PnPEntity WHERE ConfigManagerErrorCode <> 0")
        .map_err(|e| format!("não consegui listar os dispositivos: {e}"))?;
    Ok(list
        .into_iter()
        .map(|d| ProblemDevice {
            name: d.name.unwrap_or_else(|| "Dispositivo desconhecido".into()),
            class: d.pnp_class.unwrap_or_default(),
            error_code: d.config_manager_error_code.unwrap_or(0),
        })
        .collect())
}

// ---------------------------------------------------------------- Sensores

#[derive(Deserialize)]
#[serde(rename = "Sensor", rename_all = "PascalCase")]
struct HwSensor {
    name: Option<String>,
    value: Option<f32>,
    identifier: Option<String>,
}

/// O Windows não expõe a temperatura da CPU sem um driver de kernel. Se o
/// LibreHardwareMonitor (ou o antigo OpenHardwareMonitor) estiver aberto, ele publica todos os
/// sensores no WMI e a gente aproveita.
fn hardware_monitor_sensors(raw: &mut RawData) {
    for (ns, source) in
        [("ROOT\\LibreHardwareMonitor", "LibreHardwareMonitor"), ("ROOT\\OpenHardwareMonitor", "OpenHardwareMonitor")]
    {
        let Ok(conn) = WMIConnection::with_namespace_path(ns) else { continue };
        let Ok(list) = conn.raw_query::<HwSensor>(
            "SELECT Name, Value, Identifier FROM Sensor WHERE SensorType = 'Temperature'",
        ) else {
            continue;
        };
        if list.is_empty() {
            continue;
        }
        for s in list {
            let (Some(name), Some(t)) = (s.name, s.value.filter(|t| t.is_finite() && *t > 0.0)) else { continue };
            let id = s.identifier.unwrap_or_default().to_lowercase();
            let hardware = rules::hardware_from_identifier(&id);
            if hardware == "CPU" {
                raw.cpu_sensor_found = true;
            }
            // GPUs NVIDIA já vêm pelo NVML; evita linha repetida.
            if id.starts_with("/gpu-nvidia") || id.starts_with("/nvidiagpu") {
                continue;
            }
            raw.sensors.push(SensorReading {
                source: source.into(),
                label: format!("{hardware} · {name}"),
                temp_c: t,
                limit_c: None,
            });
        }
        return;
    }
}
