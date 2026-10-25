//! Índice dos apps e jogos instalados, montado a partir do Menu Iniciar, da Steam e dos
//! apelidos do `config.toml`. É montado só quando alguém pede para abrir algo e fica em
//! cache por alguns minutos, então não custa nada enquanto ninguém usa.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(600);
const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Alias,
    StartMenu,
    Steam,
}

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    /// Caminho de um `.lnk`/`.exe` ou URL (`steam://rungameid/...`), aberto pelo shell.
    pub target: String,
    pub source: Source,
}

pub struct AppIndex {
    aliases: BTreeMap<String, String>,
    entries: Vec<AppEntry>,
    built: Option<Instant>,
}

impl AppIndex {
    pub fn new(aliases: BTreeMap<String, String>) -> Self {
        Self { aliases, entries: Vec::new(), built: None }
    }

    fn refresh_if_stale(&mut self) {
        if self.built.is_some_and(|t| t.elapsed() < CACHE_TTL) {
            return;
        }
        let started = Instant::now();
        let mut entries: Vec<AppEntry> = self
            .aliases
            .iter()
            .map(|(name, target)| AppEntry { name: name.clone(), target: target.clone(), source: Source::Alias })
            .collect();
        for dir in start_menu_dirs() {
            scan_shortcuts(&dir, 0, &mut entries);
        }
        for lib in steam_libraries() {
            scan_steam_library(&lib, &mut entries);
        }
        log::info!("índice de apps: {} itens em {:?}", entries.len(), started.elapsed());
        self.entries = entries;
        self.built = Some(Instant::now());
    }

    /// Procura o app que melhor combina com o nome falado.
    pub fn find(&mut self, query: &str) -> Option<AppEntry> {
        self.refresh_if_stale();
        best_match(&self.entries, query)
    }

    /// Lista nomes que contêm o filtro (ou todos, se vazio), até `limit`.
    pub fn list(&mut self, filter: &str, limit: usize) -> Vec<String> {
        self.refresh_if_stale();
        let f = normalize(filter);
        let mut names: Vec<String> = self
            .entries
            .iter()
            .filter(|e| f.is_empty() || normalize(&e.name).contains(&f))
            .map(|e| e.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names.truncate(limit);
        names
    }

}

/// Minúsculas, sem acentos e só letras/dígitos separados por espaço.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        };
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

// Palavras que aparecem em atalhos mas não ajudam a identificar o app.
const NOISE: &[&str] = &["uninstall", "desinstalar", "readme", "help", "ajuda", "website", "manual", "documentation"];

// Palavras do pedido que não fazem parte do nome ("abre *o meu* discord").
const FILLER: &[&str] = &["o", "a", "os", "as", "um", "uma", "meu", "minha", "jogo", "app", "aplicativo", "programa", "do", "da", "de"];

fn strip_filler(q: &str) -> String {
    let words: Vec<&str> = q.split(' ').filter(|w| !FILLER.contains(w)).collect();
    if words.is_empty() {
        q.to_string()
    } else {
        words.join(" ")
    }
}

fn score(name: &str, query: &str) -> u32 {
    let (n, q) = (normalize(name), strip_filler(&normalize(query)));
    if q.is_empty() || n.is_empty() {
        return 0;
    }
    if NOISE.iter().any(|w| n.split(' ').any(|t| t == *w)) {
        return 0;
    }
    if n == q {
        return 100;
    }
    let compact = |s: &str| s.replace(' ', "");
    if compact(&n) == compact(&q) {
        return 95;
    }
    if n.starts_with(&q) {
        return 80;
    }
    let qw: Vec<&str> = q.split(' ').collect();
    let nw: Vec<&str> = n.split(' ').collect();
    // Iniciais: "vs code" / "vscode" -> "visual studio code".
    let initials: String = nw.iter().filter_map(|w| w.chars().next()).collect();
    if compact(&q).len() >= 2 && initials.starts_with(&compact(&q)) {
        return 70;
    }
    // "vscode" = iniciais de "visual studio" + "code".
    for k in 2..nw.len() {
        let head: String = nw[..k].iter().filter_map(|w| w.chars().next()).collect();
        if format!("{head}{}", nw[k..].join("")) == compact(&q) {
            return 75;
        }
    }
    // Palavra do pedido que é o começo de uma palavra do nome, ou as iniciais de palavras
    // seguidas ("vs" = "visual studio").
    let word_hit = |w: &str| {
        w.len() >= 2
            && (nw.iter().any(|x| x.starts_with(w))
                || (0..nw.len()).any(|i| {
                    let ini: String = nw[i..].iter().take(w.len()).filter_map(|x| x.chars().next()).collect();
                    ini == w
                }))
    };
    let hits = qw.iter().filter(|w| word_hit(w)).count();
    if hits == 0 {
        return 0;
    }
    // Proporção das palavras pedidas que bateram, com leve preferência por nomes curtos.
    let ratio = hits as f32 / qw.len() as f32;
    let len_penalty = (nw.len().saturating_sub(qw.len()) as f32 * 3.0).min(20.0);
    ((ratio * 75.0) - len_penalty).max(1.0) as u32
}

pub fn best_match(entries: &[AppEntry], query: &str) -> Option<AppEntry> {
    entries
        .iter()
        .map(|e| {
            // Apelidos do usuário vencem empates.
            let bonus = if e.source == Source::Alias { 5 } else { 0 };
            (score(&e.name, query) + bonus, e)
        })
        .filter(|(s, _)| *s >= 40)
        .max_by_key(|(s, _)| *s)
        .map(|(_, e)| e.clone())
}

fn start_menu_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(p) = std::env::var_os("ProgramData") {
        dirs.push(PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(p) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(p) = std::env::var_os("USERPROFILE") {
        dirs.push(PathBuf::from(p).join("Desktop"));
    }
    if let Some(p) = std::env::var_os("PUBLIC") {
        dirs.push(PathBuf::from(p).join("Desktop"));
    }
    dirs
}

fn scan_shortcuts(dir: &Path, depth: usize, out: &mut Vec<AppEntry>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < MAX_DEPTH {
                scan_shortcuts(&path, depth + 1, out);
            }
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if !matches!(ext.as_str(), "lnk" | "url" | "exe") {
            continue;
        }
        if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
            out.push(AppEntry { name: name.to_string(), target: path.display().to_string(), source: Source::StartMenu });
        }
    }
}

fn steam_libraries() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(p) = std::env::var_os(var) {
            roots.push(PathBuf::from(p).join("Steam"));
        }
    }
    let mut libs = Vec::new();
    for root in roots {
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(text) = std::fs::read_to_string(&vdf) {
            libs.extend(vdf_values(&text, "path").into_iter().map(|p| PathBuf::from(p.replace("\\\\", "\\"))));
        }
        libs.push(root);
    }
    libs.sort();
    libs.dedup();
    libs
}

fn scan_steam_library(lib: &Path, out: &mut Vec<AppEntry>) {
    let Ok(read) = std::fs::read_dir(lib.join("steamapps")) else { return };
    for entry in read.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        if !(fname.starts_with("appmanifest_") && fname.ends_with(".acf")) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            if let Some(e) = parse_app_manifest(&text) {
                out.push(e);
            }
        }
    }
}

/// Valores de uma chave em um arquivo VDF/ACF da Steam (`"chave"  "valor"`).
fn vdf_values(text: &str, key: &str) -> Vec<String> {
    let quoted = format!("\"{key}\"");
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix(&quoted)?;
            let v = rest.trim();
            Some(v.strip_prefix('"')?.strip_suffix('"')?.to_string())
        })
        .collect()
}

pub fn parse_app_manifest(text: &str) -> Option<AppEntry> {
    let appid = vdf_values(text, "appid").into_iter().next()?;
    let name = vdf_values(text, "name").into_iter().next()?;
    // Ferramentas da própria Steam (redistribuíveis, Proton etc.) não são jogos.
    if name.starts_with("Steamworks") || name.contains("Redistributable") {
        return None;
    }
    Some(AppEntry { name, target: format!("steam://rungameid/{appid}"), source: Source::Steam })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, source: Source) -> AppEntry {
        AppEntry { name: name.into(), target: format!("x:{name}"), source }
    }

    #[test]
    fn normaliza_acentos_e_pontuacao() {
        assert_eq!(normalize("  Músicas — Ação! "), "musicas acao");
    }

    #[test]
    fn encontra_pelo_nome_parcial_e_iniciais() {
        let entries = vec![
            e("Visual Studio Code", Source::StartMenu),
            e("Uninstall Visual Studio Code", Source::StartMenu),
            e("Google Chrome", Source::StartMenu),
            e("Counter-Strike 2", Source::Steam),
            e("Discord", Source::StartMenu),
        ];
        assert_eq!(best_match(&entries, "chrome").unwrap().name, "Google Chrome");
        assert_eq!(best_match(&entries, "vs code").unwrap().name, "Visual Studio Code");
        assert_eq!(best_match(&entries, "vscode").unwrap().name, "Visual Studio Code");
        assert_eq!(best_match(&entries, "counter strike").unwrap().name, "Counter-Strike 2");
        assert_eq!(best_match(&entries, "o discord").unwrap().name, "Discord");
        assert!(best_match(&entries, "photoshop").is_none());
    }

    #[test]
    fn apelido_vence_empate() {
        let entries = vec![e("cs", Source::Alias), e("CS", Source::StartMenu)];
        assert_eq!(best_match(&entries, "cs").unwrap().source, Source::Alias);
    }

    #[test]
    fn le_manifesto_da_steam() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"730\"\n\t\"name\"\t\t\"Counter-Strike 2\"\n}\n";
        let app = parse_app_manifest(acf).unwrap();
        assert_eq!(app.name, "Counter-Strike 2");
        assert_eq!(app.target, "steam://rungameid/730");
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n";
        assert_eq!(vdf_values(vdf, "path"), vec!["D:\\\\SteamLibrary".to_string()]);
    }
}
