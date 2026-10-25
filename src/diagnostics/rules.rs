//! Regras que transformam os dados brutos em problemas explicados em português, com uma
//! sugestão do que fazer. Tudo aqui é puro (sem acesso ao sistema) para poder ser testado.

use std::collections::HashMap;

use super::collect::{DiskHealth, DriverInfo, EventRecord, ProblemDevice, RawData, VolumeSpace};
use super::{ago, Category, Finding, Level, Report, SensorReading};
use crate::config::{DiagnosticsConfig, MonitorConfig};

// ---------------------------------------------------------------- Log de Eventos

struct EventRule {
    /// Pares (origem, IDs). Lista de IDs vazia vale qualquer evento da origem.
    sources: &'static [(&'static str, &'static [u32])],
    level: Level,
    /// A partir de quantas ocorrências a gravidade sobe um nível.
    escalate_at: usize,
    title: &'static str,
    suggestion: &'static str,
    /// Mostra um trecho da mensagem original (útil para códigos de tela azul).
    show_message: bool,
}

static EVENT_RULES: &[EventRule] = &[
    EventRule {
        sources: &[("BugCheck", &[1001]), ("Microsoft-Windows-WER-SystemErrorReporting", &[1001])],
        level: Level::Critico,
        escalate_at: usize::MAX,
        title: "Telas azuis (BSOD)",
        suggestion: "Os arquivos de despejo ficam em C:\\Windows\\Minidump. As causas mais comuns são driver de vídeo \
                     ou de rede, RAM instável (XMP ou overclock) e disco com defeito. Posso analisar o código do erro \
                     com você.",
        show_message: true,
    },
    EventRule {
        sources: &[("Microsoft-Windows-WHEA-Logger", &[])],
        level: Level::Critico,
        escalate_at: usize::MAX,
        title: "Erros de hardware (WHEA)",
        suggestion: "O processador registrou um erro de hardware. Quase sempre é overclock, XMP ou undervolt \
                     instável, ou problema no barramento PCIe. Volte a BIOS para o padrão e teste a memória \
                     (MemTest86).",
        show_message: true,
    },
    EventRule {
        sources: &[("disk", &[7])],
        level: Level::Critico,
        escalate_at: usize::MAX,
        title: "Setores defeituosos no disco",
        suggestion: "Faça backup agora e confira o disco no CrystalDiskInfo. Setor defeituoso costuma piorar com o \
                     tempo.",
        show_message: true,
    },
    EventRule {
        sources: &[("Microsoft-Windows-Kernel-Power", &[41]), ("EventLog", &[6008])],
        level: Level::Aviso,
        escalate_at: 3,
        title: "Desligamentos inesperados",
        suggestion: "O PC desligou ou reiniciou sem passar pelo Windows. Causas comuns: queda de energia, fonte fraca \
                     ou com defeito, superaquecimento ou travamento total. Se acontece jogando, confira as \
                     temperaturas e a fonte.",
        show_message: false,
    },
    EventRule {
        sources: &[
            ("disk", &[11, 15, 51, 52, 153]),
            ("storahci", &[]),
            ("stornvme", &[]),
            ("storport", &[]),
            ("iaStorA", &[]),
            ("iaStorAC", &[]),
            ("iaStorAVC", &[]),
            ("nvme", &[]),
        ],
        level: Level::Aviso,
        escalate_at: 20,
        title: "Erros de leitura e escrita no disco",
        suggestion: "Pode ser cabo SATA solto, porta com mau contato, firmware do SSD desatualizado ou o disco \
                     começando a falhar. Confira a saúde dele no CrystalDiskInfo.",
        show_message: true,
    },
    EventRule {
        sources: &[("Ntfs", &[55, 98, 137, 140]), ("Microsoft-Windows-Ntfs", &[55, 98, 137, 140])],
        level: Level::Aviso,
        escalate_at: usize::MAX,
        title: "Sistema de arquivos com erros",
        suggestion: "Abra o Terminal como administrador e rode \"chkdsk C: /scan\" (troque C: pela unidade \
                     citada).",
        show_message: true,
    },
    EventRule {
        sources: &[("Display", &[4101]), ("nvlddmkm", &[]), ("amdkmdag", &[]), ("amdwddmg", &[])],
        level: Level::Aviso,
        escalate_at: 5,
        title: "O driver de vídeo travou",
        suggestion: "Reinstale o driver de vídeo do zero (DDU e depois a versão mais recente), desfaça overclock \
                     da GPU e confira a temperatura dela durante o jogo.",
        show_message: false,
    },
    EventRule {
        sources: &[("volmgr", &[161, 162])],
        level: Level::Info,
        escalate_at: usize::MAX,
        title: "O Windows não conseguiu salvar o despejo de memória",
        suggestion: "Deixe o arquivo de paginação ativo no disco C:, senão não dá para descobrir a causa de telas \
                     azuis.",
        show_message: false,
    },
    EventRule {
        sources: &[("Service Control Manager", &[7000, 7009, 7011, 7022, 7023, 7024, 7031, 7034, 7038, 7043])],
        level: Level::Info,
        escalate_at: 15,
        title: "Serviços do Windows falhando",
        suggestion: "Alguns serviços não iniciaram ou pararam sozinhos. Costuma ser programa desinstalado pela \
                     metade; peça para eu explicar qualquer um deles.",
        show_message: true,
    },
    EventRule {
        sources: &[("Application Error", &[1000]), ("Application Hang", &[1002])],
        level: Level::Info,
        escalate_at: 5,
        title: "Programas fechando sozinhos ou travando",
        suggestion: "Se for sempre o mesmo programa, reinstale ou atualize ele. Se forem vários diferentes, \
                     desconfie de RAM instável ou overclock.",
        show_message: false,
    },
];

/// Ruído conhecido: aparece em todo Windows saudável e só assusta.
const IGNORED_PROVIDERS: [&str; 4] =
    ["Microsoft-Windows-DistributedCOM", "Microsoft-Windows-Perflib", "Microsoft-Windows-Security-SPP", "ESENT"];

/// Eventos cuja mensagem precisa ser lida em todas as ocorrências (não só na mais recente).
pub fn needs_every_message(provider: &str, id: u32) -> bool {
    (provider.eq_ignore_ascii_case("Application Error") && id == 1000)
        || (provider.eq_ignore_ascii_case("Application Hang") && id == 1002)
}

fn rule_for(ev: &EventRecord) -> Option<&'static EventRule> {
    EVENT_RULES.iter().find(|r| {
        r.sources.iter().any(|(p, ids)| p.eq_ignore_ascii_case(&ev.provider) && (ids.is_empty() || ids.contains(&ev.event_id)))
    })
}

fn escalate(level: Level) -> Level {
    match level {
        Level::Info => Level::Aviso,
        _ => Level::Critico,
    }
}

/// Nome do executável citado na mensagem: "Nome do aplicativo com falha: jogo.exe, ..." -> "jogo.exe".
pub fn exe_in_message(msg: &str) -> Option<String> {
    msg.split(|c: char| c.is_whitespace() || matches!(c, ',' | ':' | ';' | '"' | '\'' | '(' | ')'))
        .find(|t| t.len() > 4 && t.to_ascii_lowercase().ends_with(".exe"))
        .map(str::to_string)
}

fn excerpt(msg: &str, max: usize) -> String {
    if msg.chars().count() <= max {
        msg.to_string()
    } else {
        format!("{}…", msg.chars().take(max).collect::<String>().trim_end())
    }
}

fn times(n: usize) -> String {
    if n == 1 {
        "1 vez".into()
    } else {
        format!("{n} vezes")
    }
}

fn analyze_events(events: &[EventRecord], days: u32, now: i64, out: &mut Vec<Finding>) {
    // Por regra: contagem por (origem, id), evento mais recente e programas citados.
    #[derive(Default)]
    struct Group<'a> {
        per_source: HashMap<(String, u32), usize>,
        latest: Option<&'a EventRecord>,
        apps: HashMap<String, usize>,
    }
    let mut by_rule: Vec<Group> = EVENT_RULES.iter().map(|_| Group::default()).collect();
    let mut generic: HashMap<(String, u32), (usize, &EventRecord)> = HashMap::new();

    for ev in events {
        if IGNORED_PROVIDERS.iter().any(|p| p.eq_ignore_ascii_case(&ev.provider)) {
            continue;
        }
        match rule_for(ev) {
            Some(rule) => {
                let idx = EVENT_RULES.iter().position(|r| std::ptr::eq(r, rule)).unwrap();
                let g = &mut by_rule[idx];
                *g.per_source.entry((ev.provider.to_lowercase(), ev.event_id)).or_default() += 1;
                if g.latest.is_none_or(|l| ev.time_unix > l.time_unix || (l.message.is_empty() && !ev.message.is_empty())) {
                    g.latest = Some(ev);
                }
                if let Some(exe) = exe_in_message(&ev.message) {
                    *g.apps.entry(exe).or_default() += 1;
                }
            }
            None => {
                let e = generic.entry((ev.provider.clone(), ev.event_id)).or_insert((0, ev));
                e.0 += 1;
                if ev.time_unix > e.1.time_unix && !ev.message.is_empty() {
                    e.1 = ev;
                }
            }
        }
    }

    for (rule, g) in EVENT_RULES.iter().zip(by_rule) {
        let Some(latest) = g.latest else { continue };
        // Kernel-Power 41 e EventLog 6008 descrevem o mesmo desligamento: conta o maior, não a soma.
        let count = g.per_source.values().copied().max().unwrap_or(0);
        let level = if count >= rule.escalate_at { escalate(rule.level) } else { rule.level };
        let mut detail =
            format!("{} nos últimos {days} dias, a última {}.", times(count), ago(now - latest.time_unix));
        if !g.apps.is_empty() {
            let mut apps: Vec<_> = g.apps.into_iter().collect();
            apps.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let list: Vec<String> = apps.iter().take(4).map(|(a, n)| format!("{a} ({n}x)")).collect();
            detail.push_str(&format!(" Programas: {}.", list.join(", ")));
        }
        if rule.show_message && !latest.message.is_empty() {
            detail.push_str(&format!(" Mensagem: {}", excerpt(&latest.message, 220)));
        }
        out.push(Finding {
            level,
            category: Category::Eventos,
            title: rule.title.into(),
            detail,
            suggestion: rule.suggestion.into(),
        });
    }

    // Erros sem regra própria: só os que se repetem, para não virar uma lista enorme.
    let mut generic: Vec<_> = generic.into_iter().filter(|(_, (n, _))| *n >= 3).collect();
    generic.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(&b.0)));
    for ((provider, id), (count, ev)) in generic.into_iter().take(5) {
        let mut detail = format!("{} nos últimos {days} dias, a última {}.", times(count), ago(now - ev.time_unix));
        if !ev.message.is_empty() {
            detail.push_str(&format!(" Mensagem: {}", excerpt(&ev.message, 220)));
        }
        out.push(Finding {
            level: if ev.level == 1 { Level::Aviso } else { Level::Info },
            category: Category::Eventos,
            title: format!("Erro repetido: {provider} (evento {id})"),
            detail,
            suggestion: "Pergunte para mim sobre esse erro que eu explico o que ele significa.".into(),
        });
    }
}

// ---------------------------------------------------------------- Discos

fn analyze_disks(disks: &[DiskHealth], out: &mut Vec<Finding>) {
    for d in disks {
        let name = &d.name;
        let mut push = |level, title: String, detail: String, suggestion: &str| {
            out.push(Finding { level, category: Category::Discos, title, detail, suggestion: suggestion.into() })
        };
        if d.predict_failure == Some(true) {
            push(
                Level::Critico,
                format!("O disco {name} avisa que vai falhar"),
                "O SMART do próprio disco prevê falha em breve.".into(),
                "Faça backup imediatamente e planeje a troca do disco.",
            );
        }
        match d.health {
            Some(2) => push(
                Level::Critico,
                format!("Disco {name} com defeito"),
                "O Windows marca este disco como não íntegro.".into(),
                "Faça backup agora e substitua o disco.",
            ),
            Some(1) => push(
                Level::Aviso,
                format!("Disco {name} pede atenção"),
                "O Windows marca este disco com um aviso de saúde.".into(),
                "Confira os detalhes no CrystalDiskInfo e mantenha o backup em dia.",
            ),
            _ => {}
        }
        if let Some(w) = d.wear_pct.filter(|w| *w >= 70) {
            push(
                if w >= 90 { Level::Critico } else { Level::Aviso },
                format!("SSD {name} com {w}% da vida útil usada"),
                d.power_on_hours.map(|h| format!("Ligado por {h} horas no total.")).unwrap_or_default(),
                "SSD perto do fim da vida útil pode virar só leitura de repente. Planeje a troca.",
            );
        }
        let errors = d.read_errors_uncorrected.unwrap_or(0) + d.write_errors_uncorrected.unwrap_or(0);
        if errors > 0 {
            push(
                Level::Aviso,
                format!("Disco {name} com {errors} erros não corrigidos"),
                "O disco não conseguiu ler ou gravar alguns dados.".into(),
                "Mantenha o backup em dia e acompanhe: se o número crescer, troque o disco.",
            );
        }
        if let Some(t) = d.temp_c {
            let limit = d.temp_max_c.unwrap_or(if d.media == "HDD" { 55.0 } else { 70.0 });
            if t >= limit {
                push(
                    Level::Aviso,
                    format!("Disco {name} quente: {t:.0} °C"),
                    format!("O limite recomendado é {limit:.0} °C."),
                    "Melhore a ventilação do gabinete. Em SSD NVMe, um dissipador resolve.",
                );
            }
        }
    }
}

fn analyze_volumes(volumes: &[VolumeSpace], monitor: &MonitorConfig, out: &mut Vec<Finding>) {
    for v in volumes.iter().filter(|v| v.total_gb > 0) {
        let free_pct = v.free_gb as f32 * 100.0 / v.total_gb as f32;
        if free_pct < monitor.disk_free_alert_pct {
            out.push(Finding {
                level: if free_pct < 3.0 { Level::Critico } else { Level::Aviso },
                category: Category::Discos,
                title: format!("Unidade {} com só {free_pct:.0}% livre", v.mount),
                detail: format!("{} GB livres de {} GB.", v.free_gb, v.total_gb),
                suggestion: "Rode a Limpeza de Disco, esvazie a pasta Downloads ou mova jogos para outra unidade.".into(),
            });
        }
    }
}

// ---------------------------------------------------------------- Drivers

fn device_error_text(code: u32) -> String {
    match code {
        1 | 3 | 18 | 31 | 39 => "o driver não carrega ou está corrompido".into(),
        10 => "o dispositivo não consegue iniciar".into(),
        12 => "faltam recursos livres para o dispositivo".into(),
        28 => "nenhum driver instalado".into(),
        43 => "o Windows desligou o dispositivo porque ele relatou falha".into(),
        52 => "a assinatura do driver não foi verificada".into(),
        n => format!("erro {n} no Gerenciador de Dispositivos"),
    }
}

fn analyze_drivers(
    drivers: &[DriverInfo],
    problems: &[ProblemDevice],
    cfg: &DiagnosticsConfig,
    now: i64,
    out: &mut Vec<Finding>,
) {
    // 22 = desativado pelo usuário, 45 = não está conectado agora: não são problemas.
    let real: Vec<_> = problems.iter().filter(|p| !matches!(p.error_code, 22 | 45)).collect();
    for p in real.iter().take(6) {
        out.push(Finding {
            level: Level::Aviso,
            category: Category::Drivers,
            title: format!("Dispositivo com problema: {}", p.name),
            detail: format!("{}{}.", capitalize(&device_error_text(p.error_code)), class_suffix(&p.class)),
            suggestion: format!(
                "Abra o Gerenciador de Dispositivos, clique com o botão direito em \"{}\" e atualize ou reinstale o \
                 driver.",
                p.name
            ),
        });
    }
    if real.len() > 6 {
        out.push(Finding {
            level: Level::Aviso,
            category: Category::Drivers,
            title: format!("Mais {} dispositivos com problema", real.len() - 6),
            detail: real[6..].iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "),
            suggestion: "Abra o Gerenciador de Dispositivos e procure os ícones de alerta amarelos.".into(),
        });
    }

    let today = now.div_euclid(86_400);
    let mut seen = std::collections::HashSet::new();
    let mut old: Vec<(&DriverInfo, i64)> = Vec::new();
    for d in drivers {
        let provider = d.provider.to_lowercase();
        if provider.is_empty() || provider.contains("microsoft") || !seen.insert((&d.device, &d.version)) {
            continue;
        }
        let Some((y, m, day)) = d.date else { continue };
        let age_days = today - days_from_civil(y, m, day);
        let class = d.class.to_ascii_uppercase();
        if class == "DISPLAY" && age_days > 365 {
            out.push(Finding {
                level: Level::Aviso,
                category: Category::Drivers,
                title: format!("Driver de vídeo antigo: {}", d.device),
                detail: format!("Versão {} de {:02}/{:02}/{}.", d.version, day, m, y),
                suggestion: "Jogos novos costumam precisar de driver recente. Baixe a versão atual no site da \
                             NVIDIA, AMD ou Intel."
                    .into(),
            });
        } else if IMPORTANT_CLASSES.contains(&class.as_str()) && age_days > i64::from(cfg.old_driver_years) * 365 {
            old.push((d, age_days));
        }
    }
    if !old.is_empty() {
        old.sort_by(|a, b| b.1.cmp(&a.1));
        let list: Vec<String> = old
            .iter()
            .take(6)
            .map(|(d, _)| match d.date {
                Some((y, _, _)) => format!("{} ({y})", d.device),
                None => d.device.clone(),
            })
            .collect();
        out.push(Finding {
            level: Level::Info,
            category: Category::Drivers,
            title: format!("{} driver{} com mais de {} anos", old.len(), if old.len() == 1 { "" } else { "s" }, cfg.old_driver_years),
            detail: format!("{}.", list.join(", ")),
            suggestion: "Não consulto a internet; a idade é só um indício. Veja no site do fabricante da placa-mãe \
                         se há versões novas de chipset, rede e áudio."
                .into(),
        });
    }
}

const IMPORTANT_CLASSES: [&str; 7] = ["NET", "MEDIA", "BLUETOOTH", "HDC", "SCSIADAPTER", "SYSTEM", "USB"];

fn class_suffix(class: &str) -> String {
    if class.is_empty() {
        String::new()
    } else {
        format!(" (classe {class})")
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

// ---------------------------------------------------------------- Temperaturas

fn analyze_sensors(raw: &RawData, monitor: &MonitorConfig, out: &mut Vec<Finding>) {
    for s in &raw.sensors {
        let limit = s.limit_c.unwrap_or(monitor.temp_alert_c);
        let level = if s.temp_c >= limit {
            Level::Critico
        } else if s.temp_c >= limit - 10.0 {
            Level::Aviso
        } else {
            continue;
        };
        out.push(Finding {
            level,
            category: Category::Temperaturas,
            title: format!("{} a {:.0} °C", s.label, s.temp_c),
            detail: format!("Limite considerado: {limit:.0} °C ({}).", s.source),
            suggestion: "Limpe a poeira dos filtros e dissipadores, confira se as ventoinhas giram e, se a pasta \
                         térmica tiver mais de 3 anos, troque."
                .into(),
        });
    }
    if cfg!(windows) && !raw.cpu_sensor_found {
        out.push(Finding {
            level: Level::Info,
            category: Category::Temperaturas,
            title: "Não consigo ler a temperatura da CPU".into(),
            detail: "O Windows não mostra esse sensor para programas comuns.".into(),
            suggestion: "Instale o LibreHardwareMonitor e deixe ele aberto (pode ser minimizado na bandeja); eu leio \
                         todos os sensores dele automaticamente."
                .into(),
        });
    }
}

// ---------------------------------------------------------------- Montagem do relatório

pub fn analyze(raw: &RawData, cfg: &DiagnosticsConfig, monitor: &MonitorConfig, now: i64) -> Report {
    let mut findings = Vec::new();
    let mut notes = raw.notes.clone();

    match &raw.events {
        Ok(events) => analyze_events(events, cfg.event_days, now, &mut findings),
        Err(e) => notes.push(format!("Log de eventos: {e}.")),
    }
    let mut sensors = raw.sensors.clone();
    match &raw.disks {
        Ok(disks) => {
            analyze_disks(disks, &mut findings);
            sensors.extend(disks.iter().filter_map(|d| {
                Some(SensorReading { source: "Disco".into(), label: d.name.clone(), temp_c: d.temp_c?, limit_c: d.temp_max_c })
            }));
        }
        Err(e) => notes.push(format!("Saúde dos discos: {e}.")),
    }
    analyze_volumes(&raw.volumes, monitor, &mut findings);
    match (&raw.drivers, &raw.problem_devices) {
        (Ok(d), Ok(p)) => analyze_drivers(d, p, cfg, now, &mut findings),
        (Ok(d), Err(e)) => {
            notes.push(format!("Dispositivos: {e}."));
            analyze_drivers(d, &[], cfg, now, &mut findings);
        }
        (Err(e), Ok(p)) => {
            notes.push(format!("Drivers: {e}."));
            analyze_drivers(&[], p, cfg, now, &mut findings);
        }
        (Err(e), Err(_)) => notes.push(format!("Drivers: {e}.")),
    }
    analyze_sensors(raw, monitor, &mut findings);

    let days_up = raw.uptime_s / 86_400;
    if days_up >= 7 {
        findings.push(Finding {
            level: Level::Info,
            category: Category::Sistema,
            title: format!("PC ligado há {days_up} dias sem reiniciar"),
            detail: "Com a Inicialização Rápida do Windows, desligar não zera esse tempo; só Reiniciar zera.".into(),
            suggestion: "Reinicie de vez em quando para aplicar atualizações e liberar memória.".into(),
        });
    }

    findings.sort_by(|a, b| b.level.cmp(&a.level));
    sensors.sort_by(|a, b| b.temp_c.total_cmp(&a.temp_c));
    Report { generated_unix: now, duration_ms: 0, findings, sensors, notes, created: None }
}

// ---------------------------------------------------------------- Utilitários

/// Data do WMI ("20230115000000.******+000") como (ano, mês, dia).
pub fn parse_wmi_date(s: &str) -> Option<(i32, u32, u32)> {
    let y = s.get(0..4)?.parse().ok()?;
    let m = s.get(4..6)?.parse().ok()?;
    let d = s.get(6..8)?.parse().ok()?;
    ((1980..=2100).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d)).then_some((y, m, d))
}

/// Dias desde 1970-01-01 (algoritmo de Howard Hinnant).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "SCSI\Disk&Ven_Samsung&Prod_SSD_970_EVO\4&1a2b&0_0" -> "Samsung SSD 970 EVO".
pub fn pretty_instance_name(instance: &str) -> String {
    let mut ven = None;
    let mut prod = None;
    for part in instance.split(['\\', '&']) {
        if let Some(v) = part.strip_prefix("Ven_") {
            ven = Some(v.replace('_', " "));
        } else if let Some(p) = part.strip_prefix("Prod_") {
            prod = Some(p.replace('_', " "));
        }
    }
    match (ven, prod) {
        (Some(v), Some(p)) => format!("{} {}", v.trim(), p.trim()),
        (None, Some(p)) => p.trim().to_string(),
        _ => instance.to_string(),
    }
}

/// Tipo de hardware a partir do identificador do LibreHardwareMonitor ("/amdcpu/0/temperature/2").
pub fn hardware_from_identifier(id: &str) -> &'static str {
    let first = id.trim_start_matches('/').split('/').next().unwrap_or_default();
    if first.contains("cpu") {
        "CPU"
    } else if first.contains("gpu") {
        "GPU"
    } else if ["hdd", "ssd", "nvme", "storage"].iter().any(|k| first.contains(k)) {
        "Disco"
    } else if ["lpc", "mainboard", "motherboard", "superio"].iter().any(|k| first.contains(k)) {
        "Placa-mãe"
    } else if first.contains("ram") || first.contains("memory") {
        "Memória"
    } else {
        "Sensor"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_000_000;

    fn ev(provider: &str, id: u32, ago_s: i64, msg: &str) -> EventRecord {
        EventRecord { provider: provider.into(), event_id: id, level: 2, time_unix: NOW - ago_s, message: msg.into() }
    }

    fn raw() -> RawData {
        RawData {
            events: Ok(vec![]),
            disks: Ok(vec![]),
            volumes: vec![],
            drivers: Ok(vec![]),
            problem_devices: Ok(vec![]),
            sensors: vec![],
            cpu_sensor_found: true,
            uptime_s: 3600,
            notes: vec![],
        }
    }

    fn run(raw: &RawData) -> Report {
        analyze(raw, &DiagnosticsConfig::default(), &MonitorConfig::default(), NOW)
    }

    #[test]
    fn healthy_pc_has_no_findings() {
        let r = run(&raw());
        assert!(r.findings.is_empty(), "{:?}", r.findings);
    }

    #[test]
    fn unexpected_shutdowns_are_counted_once_and_escalate() {
        let mut r = raw();
        r.events = Ok(vec![
            ev("Microsoft-Windows-Kernel-Power", 41, 3600, ""),
            ev("EventLog", 6008, 3600, ""),
            ev("Microsoft-Windows-Kernel-Power", 41, 7200, ""),
        ]);
        let rep = run(&r);
        assert_eq!(rep.findings.len(), 1);
        let f = &rep.findings[0];
        assert_eq!(f.title, "Desligamentos inesperados");
        assert_eq!(f.level, Level::Aviso);
        assert!(f.detail.starts_with("2 vezes nos últimos 7 dias, a última há 1 h."), "{}", f.detail);

        let mut r = raw();
        r.events = Ok((0..3).map(|i| ev("Microsoft-Windows-Kernel-Power", 41, i * 100, "")).collect());
        assert_eq!(run(&r).findings[0].level, Level::Critico);
    }

    #[test]
    fn bsod_shows_bugcheck_message() {
        let mut r = raw();
        r.events = Ok(vec![ev("Microsoft-Windows-WER-SystemErrorReporting", 1001, 86_400 * 2, "O computador foi reinicializado após uma verificação de erro. A verificação de erro foi: 0x0000001a")]);
        let f = &run(&r).findings[0];
        assert_eq!(f.level, Level::Critico);
        assert!(f.detail.contains("0x0000001a"));
        assert!(f.detail.contains("há 2 dias"));
    }

    #[test]
    fn app_crashes_list_programs() {
        let mut r = raw();
        r.events = Ok(vec![
            ev("Application Error", 1000, 10, "Nome do aplicativo com falha: Discord.exe, versão: 1.0"),
            ev("Application Error", 1000, 20, "Faulting application name: Discord.exe, version: 1.0"),
            ev("Application Hang", 1002, 30, "O programa chrome.exe versão 120 parou de interagir"),
        ]);
        let f = &run(&r).findings[0];
        assert_eq!(f.title, "Programas fechando sozinhos ou travando");
        assert_eq!(f.level, Level::Info);
        assert!(f.detail.contains("Discord.exe (2x), chrome.exe (1x)"), "{}", f.detail);
    }

    #[test]
    fn noise_is_ignored_and_generic_errors_need_repetition() {
        let mut r = raw();
        let mut events: Vec<_> = (0..50).map(|_| ev("Microsoft-Windows-DistributedCOM", 10016, 10, "")).collect();
        events.push(ev("AlgumDriver", 5, 10, "uma vez só"));
        events.extend((0..3).map(|_| ev("OutroDriver", 9, 10, "falhou")));
        r.events = Ok(events);
        let rep = run(&r);
        assert_eq!(rep.findings.len(), 1);
        assert_eq!(rep.findings[0].title, "Erro repetido: OutroDriver (evento 9)");
    }

    #[test]
    fn disk_health_rules() {
        let mut r = raw();
        r.disks = Ok(vec![
            DiskHealth { name: "Samsung 970".into(), media: "SSD".into(), health: Some(0), wear_pct: Some(92), ..Default::default() },
            DiskHealth { name: "WD Blue".into(), media: "HDD".into(), health: Some(1), temp_c: Some(58.0), predict_failure: Some(true), ..Default::default() },
        ]);
        let rep = run(&r);
        let titles: Vec<_> = rep.findings.iter().map(|f| (f.level, f.title.as_str())).collect();
        assert!(titles.contains(&(Level::Critico, "SSD Samsung 970 com 92% da vida útil usada")));
        assert!(titles.contains(&(Level::Critico, "O disco WD Blue avisa que vai falhar")));
        assert!(titles.contains(&(Level::Aviso, "Disco WD Blue pede atenção")));
        assert!(titles.contains(&(Level::Aviso, "Disco WD Blue quente: 58 °C")));
        assert_eq!(rep.findings[0].level, Level::Critico, "críticos vêm primeiro");
        assert_eq!(rep.sensors.len(), 1, "temperatura do disco vira sensor");
    }

    #[test]
    fn low_free_space() {
        let mut r = raw();
        r.volumes = vec![VolumeSpace { mount: "C:\\".into(), total_gb: 500, free_gb: 10 }];
        let f = &run(&r).findings[0];
        assert_eq!(f.level, Level::Critico);
        assert_eq!(f.title, "Unidade C:\\ com só 2% livre");
    }

    #[test]
    fn drivers_and_devices() {
        let mut r = raw();
        let drv = |device: &str, class: &str, provider: &str, date| DriverInfo {
            device: device.into(),
            class: class.into(),
            provider: provider.into(),
            version: "1.0".into(),
            date: Some(date),
        };
        r.drivers = Ok(vec![
            drv("NVIDIA GeForce RTX 3070", "DISPLAY", "NVIDIA", (2024, 1, 10)),
            drv("Realtek PCIe GbE", "NET", "Realtek", (2019, 5, 2)),
            drv("Realtek PCIe GbE", "NET", "Realtek", (2019, 5, 2)),
            drv("Controlador genérico", "SYSTEM", "Microsoft", (2006, 6, 21)),
            drv("Mouse", "MOUSE", "Logitech", (2015, 1, 1)),
        ]);
        r.problem_devices = Ok(vec![
            ProblemDevice { name: "Controlador de áudio".into(), class: "MEDIA".into(), error_code: 28 },
            ProblemDevice { name: "Desativado".into(), class: "NET".into(), error_code: 22 },
        ]);
        let rep = run(&r);
        let titles: Vec<_> = rep.findings.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Dispositivo com problema: Controlador de áudio",
                "Driver de vídeo antigo: NVIDIA GeForce RTX 3070",
                "1 driver com mais de 3 anos",
            ]
        );
        assert_eq!(rep.findings[0].detail, "Nenhum driver instalado (classe MEDIA).");
        assert_eq!(rep.findings[2].detail, "Realtek PCIe GbE (2019).");
    }

    #[test]
    fn hot_sensors() {
        let mut r = raw();
        r.sensors = vec![
            SensorReading { source: "GPU".into(), label: "RTX 3070".into(), temp_c: 84.0, limit_c: Some(93.0) },
            SensorReading { source: "LibreHardwareMonitor".into(), label: "CPU · Package".into(), temp_c: 95.0, limit_c: None },
            SensorReading { source: "Sistema".into(), label: "ACPI".into(), temp_c: 40.0, limit_c: None },
        ];
        let rep = run(&r);
        let got: Vec<_> = rep.findings.iter().map(|f| (f.level, f.title.as_str())).collect();
        assert_eq!(got, [(Level::Critico, "CPU · Package a 95 °C"), (Level::Aviso, "RTX 3070 a 84 °C")]);
    }

    #[test]
    fn collector_errors_become_notes() {
        let mut r = raw();
        r.events = Err("acesso negado".into());
        r.uptime_s = 9 * 86_400;
        let rep = run(&r);
        assert_eq!(rep.notes, ["Log de eventos: acesso negado."]);
        assert_eq!(rep.findings[0].title, "PC ligado há 9 dias sem reiniciar");
    }

    #[test]
    fn helpers() {
        assert_eq!(parse_wmi_date("20230115000000.******+000"), Some((2023, 1, 15)));
        assert_eq!(parse_wmi_date("lixo"), None);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 10, 25), 20_751);
        assert_eq!(pretty_instance_name("SCSI\\Disk&Ven_Samsung&Prod_SSD_970_EVO\\4&1a2b&0&000000_0"), "Samsung SSD 970 EVO");
        assert_eq!(pretty_instance_name("IDE\\DiskWDC_WD10\\5&0"), "IDE\\DiskWDC_WD10\\5&0");
        assert_eq!(hardware_from_identifier("/amdcpu/0/temperature/2"), "CPU");
        assert_eq!(hardware_from_identifier("/lpc/nct6798d/temperature/1"), "Placa-mãe");
        assert_eq!(hardware_from_identifier("/nvme/0/temperature/0"), "Disco");
        assert_eq!(exe_in_message("Faulting application name: Game.exe, version"), Some("Game.exe".into()));
        assert_eq!(exe_in_message("sem programa"), None);
    }
}
