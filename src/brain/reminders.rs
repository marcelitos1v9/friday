//! Lembretes: interpretação de horários e a thread que dispara na hora certa.
//!
//! A thread dorme até o próximo lembrete (ou no máximo 1 h); não há polling.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Datelike, Duration as CDuration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike};

use super::memory::{now, Memory};

/// Calcula o horário do lembrete a partir de "daqui a N minutos" ou de um horário
/// ("18:30", "18h", "18h30", "2026-10-26 09:00", "26/10 09:00").
pub fn resolve(minutes: Option<f64>, at: Option<&str>, now: DateTime<Local>) -> Result<DateTime<Local>> {
    if let Some(m) = minutes.filter(|m| *m > 0.0) {
        return Ok(now + CDuration::seconds((m * 60.0).round() as i64));
    }
    let at = at.map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("faltou dizer quando"))?;
    parse_at(at, now)
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim().trim_end_matches("min").trim();
    let (h, m) = if let Some((h, m)) = s.split_once(':').or_else(|| s.split_once('h')) {
        (h.trim(), m.trim())
    } else {
        (s.trim_end_matches('h'), "")
    };
    let h: u32 = h.parse().ok()?;
    let m: u32 = if m.is_empty() { 0 } else { m.parse().ok()? };
    NaiveTime::from_hms_opt(h, m, 0)
}

fn parse_date(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%d/%m/%Y") {
        return Some(d);
    }
    let (d, m) = s.split_once('/')?;
    let d = NaiveDate::from_ymd_opt(today.year(), m.parse().ok()?, d.parse().ok()?)?;
    // "26/10" já passou este ano -> ano que vem.
    Some(if d < today { d.with_year(today.year() + 1)? } else { d })
}

pub fn parse_at(at: &str, now: DateTime<Local>) -> Result<DateTime<Local>> {
    let at = at.trim().trim_start_matches("às").trim_start_matches("as ").trim();
    let today = now.date_naive();
    let (date, time) = match at.rsplit_once(|c: char| c == ' ' || c == 'T') {
        Some((d, t)) => {
            let d = d.trim().trim_end_matches("às").trim_end_matches(" as").trim();
            let lower = d.to_lowercase();
            let date = if lower == "amanha" || lower == "amanhã" {
                today.succ_opt()
            } else if lower == "hoje" {
                Some(today)
            } else {
                parse_date(d, today)
            };
            (date.ok_or_else(|| anyhow!("não entendi a data \"{d}\""))?, parse_time(t))
        }
        None => (today, parse_time(at)),
    };
    let time = time.ok_or_else(|| anyhow!("não entendi o horário \"{at}\""))?;
    let mut naive = NaiveDateTime::new(date, time);
    // Só o horário e ele já passou hoje: é amanhã.
    if date == today && naive <= now.naive_local() && !at.contains(['-', '/']) {
        naive += CDuration::days(1);
    }
    match Local.from_local_datetime(&naive).earliest() {
        Some(t) if t > now => Ok(t),
        Some(_) => bail!("esse horário já passou"),
        None => bail!("horário inválido"),
    }
}

/// "hoje às 18:30", "amanhã às 09:00" ou "26/10 às 09:00".
pub fn describe(ts: i64, now: DateTime<Local>) -> String {
    let Some(t) = Local.timestamp_opt(ts, 0).single() else { return "em breve".into() };
    let hm = format!("{:02}:{:02}", t.hour(), t.minute());
    let days = (t.date_naive() - now.date_naive()).num_days();
    match days {
        0 => format!("hoje às {hm}"),
        1 => format!("amanhã às {hm}"),
        _ => format!("{:02}/{:02} às {hm}", t.day(), t.month()),
    }
}

/// Dorme até o próximo lembrete e chama `fire` com o texto de cada um que vencer.
/// Qualquer mensagem em `wake` faz a thread recalcular (ex.: lembrete novo).
pub fn spawn_scheduler(
    mem: Arc<Mutex<Memory>>,
    wake: Receiver<()>,
    fire: impl Fn(String) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("lembretes".into())
        .spawn(move || loop {
            let next = mem.lock().unwrap().next_due().unwrap_or(None);
            let wait = match next {
                Some(due) => (due - now()).clamp(0, 3600) as u64,
                None => 3600,
            };
            match wake.recv_timeout(Duration::from_secs(wait)) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let due = mem.lock().unwrap().take_due(now()).unwrap_or_default();
            for r in due {
                log::info!("lembrete: {}", r.text);
                fire(r.text);
            }
        })
        .expect("falha ao iniciar a thread de lembretes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn horarios() {
        let now = at(2026, 10, 25, 14, 0);
        assert_eq!(resolve(Some(10.0), None, now).unwrap(), at(2026, 10, 25, 14, 10));
        assert_eq!(parse_at("18:30", now).unwrap(), at(2026, 10, 25, 18, 30));
        assert_eq!(parse_at("18h", now).unwrap(), at(2026, 10, 25, 18, 0));
        assert_eq!(parse_at("às 9h15", now).unwrap(), at(2026, 10, 26, 9, 15));
        assert_eq!(parse_at("amanhã 08:00", now).unwrap(), at(2026, 10, 26, 8, 0));
        assert_eq!(parse_at("amanhã às 08:00", now).unwrap(), at(2026, 10, 26, 8, 0));
        assert_eq!(parse_at("2026-10-30 20:00", now).unwrap(), at(2026, 10, 30, 20, 0));
        assert_eq!(parse_at("01/11 07:30", now).unwrap(), at(2026, 11, 1, 7, 30));
        assert!(parse_at("2026-10-01 10:00", now).is_err());
        assert!(parse_at("banana", now).is_err());
    }

    #[test]
    fn descricao() {
        let now = at(2026, 10, 25, 14, 0);
        assert_eq!(describe(at(2026, 10, 25, 18, 30).timestamp(), now), "hoje às 18:30");
        assert_eq!(describe(at(2026, 10, 26, 8, 0).timestamp(), now), "amanhã às 08:00");
    }
}
