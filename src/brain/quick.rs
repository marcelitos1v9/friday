//! Atalho sem LLM para os comandos mais comuns ("volume 30", "pausa", "abre o discord").
//!
//! Responde na hora, não gasta rede nem GPU e funciona mesmo sem Groq/Ollama — ideal no meio
//! de uma partida. Qualquer coisa que não encaixe aqui vai para o LLM.

use crate::actions::apps::normalize;
use crate::actions::{MediaKey, VolumeAction};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Open(String),
    Volume(VolumeAction),
    Media(MediaKey),
    Close(String),
    CloseHung,
    Routine(String),
    Wait(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Act(Action),
    /// Resposta a uma pergunta de confirmação ("sim" / "não").
    Confirm(bool),
    Remind { text: String, minutes: Option<f64>, at: Option<String> },
    Time,
}

const WAKE: &[&str] = &["sexta feira", "sexta", "friday"];
const POLITE_PREFIX: &[&str] = &["por favor", "pode", "da pra", "voce pode", "consegue", "ai"];

fn strip_prefixes(mut t: &str) -> &str {
    loop {
        let before = t;
        for p in WAKE.iter().chain(POLITE_PREFIX) {
            if let Some(rest) = t.strip_prefix(p) {
                if rest.is_empty() || rest.starts_with(' ') {
                    t = rest.trim_start();
                }
            }
        }
        if t == before {
            return t;
        }
    }
}

fn clean(text: &str) -> String {
    let n = normalize(text);
    let t = strip_prefixes(&n);
    let t = t.strip_suffix(" por favor").unwrap_or(t);
    t.trim().to_string()
}

fn first_number(t: &str) -> Option<u32> {
    t.split(' ').find_map(|w| w.trim_end_matches('%').parse().ok())
}

fn after_verb<'a>(t: &'a str, verbs: &[&str]) -> Option<&'a str> {
    verbs.iter().find_map(|v| t.strip_prefix(v).and_then(|r| r.strip_prefix(' ')).map(str::trim))
}

fn strip_article(s: &str) -> &str {
    for a in ["o ", "a ", "os ", "as ", "meu ", "minha "] {
        if let Some(r) = s.strip_prefix(a) {
            return r.trim();
        }
    }
    s
}

pub fn parse(text: &str, routines: &[String]) -> Option<Intent> {
    let t = clean(text);
    if t.is_empty() {
        return None;
    }

    match t.as_str() {
        "sim" | "s" | "confirma" | "confirmo" | "pode sim" | "isso" | "manda ver" | "pode fechar" | "fecha" | "ok"
        | "beleza" | "claro" => return Some(Intent::Confirm(true)),
        "nao" | "n" | "cancela" | "cancelar" | "deixa" | "deixa pra la" | "esquece" | "nao fecha" => {
            return Some(Intent::Confirm(false))
        }
        "que horas sao" | "que hora e" | "que horas e" | "horas" | "que dia e hoje" => return Some(Intent::Time),
        _ => {}
    }

    if ["sim ", "confirma ", "pode fechar "].iter().any(|p| t.starts_with(p)) {
        return Some(Intent::Confirm(true));
    }

    if let Some(r) = parse_reminder(text) {
        return Some(r);
    }

    // Pedidos compostos ("abre o chrome e põe volume 30") ficam com o LLM.
    if t.contains(" e ") || t.contains(" depois ") {
        return None;
    }

    // Volume
    let has_volume = t.split(' ').any(|w| w == "volume" || w == "som");
    if ["muta", "mutar", "mudo", "silencia", "silencio", "fica muda", "tira o som", "corta o som"]
        .iter()
        .any(|p| t == *p || t.starts_with(&format!("{p} ")))
    {
        return Some(Intent::Act(Action::Volume(VolumeAction::Mute)));
    }
    if ["desmuta", "desmutar", "tira do mudo", "volta o som", "liga o som"].iter().any(|p| t.starts_with(p)) {
        return Some(Intent::Act(Action::Volume(VolumeAction::Unmute)));
    }
    if has_volume {
        let n = first_number(&t);
        let up = ["aumenta", "sobe", "mais alto", "aumentar", "subir"].iter().any(|w| t.contains(w));
        let down = ["abaixa", "diminui", "baixa", "mais baixo", "abaixar", "diminuir"].iter().any(|w| t.contains(w));
        let action = match (n, up, down) {
            (Some(n), false, false) => VolumeAction::Set(n.min(100) as u8),
            (n, true, false) => VolumeAction::Up(n.unwrap_or(10).min(100) as u8),
            (n, false, true) => VolumeAction::Down(n.unwrap_or(10).min(100) as u8),
            _ => return None,
        };
        return Some(Intent::Act(Action::Volume(action)));
    }

    // Mídia
    let media_obj = ["", " a musica", " o video", " a midia", " o som", " ai"];
    let is = |verbs: &[&str]| verbs.iter().any(|v| media_obj.iter().any(|o| t == format!("{v}{o}")));
    if is(&["pausa", "pausar", "pause", "play", "da play", "despausa", "continua", "continuar", "toca", "tocar", "solta"]) {
        return Some(Intent::Act(Action::Media(MediaKey::PlayPause)));
    }
    if is(&["proxima", "proximo", "pula", "pular", "passa", "proxima faixa", "skip"]) {
        return Some(Intent::Act(Action::Media(MediaKey::Next)));
    }
    if is(&["anterior", "volta", "musica anterior", "volta uma"]) {
        return Some(Intent::Act(Action::Media(MediaKey::Previous)));
    }
    if is(&["para", "parar"]) {
        return Some(Intent::Act(Action::Media(MediaKey::Stop)));
    }

    // Rotinas: "modo trabalho", "ativa o modo trabalho", "rotina trabalho"
    for prefix in ["ativa o modo ", "ativar o modo ", "ativa modo ", "liga o modo ", "modo ", "rotina ", "roda a rotina "] {
        if let Some(name) = t.strip_prefix(prefix) {
            if let Some(r) = routines.iter().find(|r| normalize(r) == name) {
                return Some(Intent::Act(Action::Routine(r.clone())));
            }
        }
    }

    if let Some(rest) = after_verb(&t, &["esperar", "espera", "aguardar"]) {
        return first_number(rest).map(|n| Intent::Act(Action::Wait(n as u64)));
    }

    if let Some(rest) = after_verb(&t, &["fecha", "fechar", "feche", "mata", "encerra", "encerrar", "finaliza"]) {
        if ["travado", "travou", "nao responde", "nao esta respondendo", "congelado"].iter().any(|w| rest.contains(w)) {
            return Some(Intent::Act(Action::CloseHung));
        }
        let name = strip_article(rest);
        return (!name.is_empty()).then(|| Intent::Act(Action::Close(name.to_string())));
    }

    if let Some(rest) = after_verb(&t, &["abre", "abrir", "abra", "inicia", "iniciar", "executa", "executar", "roda", "rodar"]) {
        let name = strip_article(rest);
        return (!name.is_empty()).then(|| Intent::Act(Action::Open(name.to_string())));
    }

    None
}

/// "me lembra de tomar água em 20 minutos", "me avisa às 18:30 pra sair".
fn parse_reminder(text: &str) -> Option<Intent> {
    let lower = text.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
    let mut body = None;
    for p in ["me lembra", "me lembre", "me avisa", "me avise", "lembrete", "cria um lembrete", "criar lembrete"] {
        if let Some(pos) = lower.find(p) {
            body = Some(lower[pos + p.len()..].trim().to_string());
            break;
        }
    }
    let body = body?;

    // Tempo relativo no fim ou no começo: "em 20 minutos", "daqui a 1 hora".
    let units: &[(&str, f64)] =
        &[("minutos", 1.0), ("minuto", 1.0), ("min", 1.0), ("horas", 60.0), ("hora", 60.0), ("segundos", 1.0 / 60.0)];
    let words: Vec<&str> = body.split_whitespace().collect();
    for i in 0..words.len() {
        let Ok(n) = words[i].replace(',', ".").parse::<f64>() else { continue };
        let Some(next) = words.get(i + 1) else { continue };
        let Some((_, mult)) = units.iter().find(|(u, _)| next == u) else { continue };
        // Palavras que introduzem o tempo ("em", "daqui a", "daqui").
        let mut start = i;
        while start > 0 && ["em", "daqui", "a", "uns", "umas"].contains(&words[start - 1]) {
            start -= 1;
        }
        let mut rest: Vec<&str> = words[..start].to_vec();
        rest.extend_from_slice(&words[i + 2..]);
        return Some(Intent::Remind { text: reminder_text(&rest), minutes: Some(n * mult), at: None });
    }

    // Horário: "às 18:30", "as 9h", "amanhã às 8".
    for i in 0..words.len() {
        if !["às", "as"].contains(&words[i]) {
            continue;
        }
        let Some(time) = words.get(i + 1) else { continue };
        if !time.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let mut start = i;
        let mut at = time.to_string();
        if start > 0 && ["amanhã", "amanha", "hoje"].contains(&words[start - 1]) {
            start -= 1;
            at = format!("{} {time}", words[start]);
        }
        let mut rest: Vec<&str> = words[..start].to_vec();
        rest.extend_from_slice(&words[i + 2..]);
        return Some(Intent::Remind { text: reminder_text(&rest), minutes: None, at: Some(at) });
    }
    None
}

fn reminder_text(words: &[&str]) -> String {
    let mut w: Vec<&str> = words.to_vec();
    while let Some(first) = w.first() {
        if ["de", "que", "pra", "para", "do", "da", ","].contains(first) {
            w.remove(0);
        } else {
            break;
        }
    }
    let t = w.join(" ").trim_matches([',', ' ']).to_string();
    if t.is_empty() {
        "lembrete".into()
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: &str) -> Option<Intent> {
        parse(t, &["trabalho".to_string()])
    }

    #[test]
    fn volume() {
        assert_eq!(p("Sexta-feira, volume 30%"), Some(Intent::Act(Action::Volume(VolumeAction::Set(30)))));
        assert_eq!(p("coloca o volume em 45"), Some(Intent::Act(Action::Volume(VolumeAction::Set(45)))));
        assert_eq!(p("aumenta o volume"), Some(Intent::Act(Action::Volume(VolumeAction::Up(10)))));
        assert_eq!(p("abaixa o som 20"), Some(Intent::Act(Action::Volume(VolumeAction::Down(20)))));
        assert_eq!(p("muta"), Some(Intent::Act(Action::Volume(VolumeAction::Mute))));
        assert_eq!(p("volta o som"), Some(Intent::Act(Action::Volume(VolumeAction::Unmute))));
    }

    #[test]
    fn midia() {
        assert_eq!(p("pausa a música"), Some(Intent::Act(Action::Media(MediaKey::PlayPause))));
        assert_eq!(p("próxima"), Some(Intent::Act(Action::Media(MediaKey::Next))));
        assert_eq!(p("música anterior"), Some(Intent::Act(Action::Media(MediaKey::Previous))));
        assert_eq!(p("toca imagine dragons no spotify"), None);
    }

    #[test]
    fn apps_rotinas_e_fechar() {
        assert_eq!(p("abre o Discord"), Some(Intent::Act(Action::Open("discord".into()))));
        assert_eq!(p("pode abrir o counter strike por favor"), Some(Intent::Act(Action::Open("counter strike".into()))));
        assert_eq!(p("ativa o modo trabalho"), Some(Intent::Act(Action::Routine("trabalho".into()))));
        assert_eq!(p("modo festa"), None);
        assert_eq!(p("fecha o chrome"), Some(Intent::Act(Action::Close("chrome".into()))));
        assert_eq!(p("fecha o programa que travou"), Some(Intent::Act(Action::CloseHung)));
        assert_eq!(p("abre o chrome e coloca volume 30"), None);
        assert_eq!(p("Sim"), Some(Intent::Confirm(true)));
        assert_eq!(p("não"), Some(Intent::Confirm(false)));
        assert_eq!(p("o que você acha do meu pc?"), None);
    }

    #[test]
    fn lembretes() {
        assert_eq!(
            p("me lembra de tirar a pizza do forno em 20 minutos"),
            Some(Intent::Remind { text: "tirar a pizza do forno".into(), minutes: Some(20.0), at: None })
        );
        assert_eq!(
            p("Sexta, me avisa daqui a 1 hora pra beber água"),
            Some(Intent::Remind { text: "beber água".into(), minutes: Some(60.0), at: None })
        );
        assert_eq!(
            p("me lembra amanhã às 9h de pagar a conta"),
            Some(Intent::Remind { text: "pagar a conta".into(), minutes: None, at: Some("amanhã 9h".into()) })
        );
        assert_eq!(p("me lembra de comprar pão"), None);
    }
}
