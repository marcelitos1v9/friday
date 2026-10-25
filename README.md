# Sexta-Feira

Assistente pessoal para Windows inspirado na Sexta-Feira do Homem de Ferro. É um app nativo em Rust, sem navegador embutido, feito para ficar ligado o tempo todo sem atrapalhar os jogos.

## O que já funciona (etapa M0)

- Janela principal estilo HUD com CPU, memória, GPU NVIDIA (uso, temperatura e VRAM) e o consumo da própria Sexta-Feira.
- Widget flutuante sem borda e sempre no topo. Arraste para mover; clique duplo abre a janela principal. A posição fica salva.
- Diagnóstico: alerta de temperatura alta, RAM quase cheia e disco com pouco espaço.
- Lista dos programas que mais usam memória, agrupados por nome.
- Modo jogo automático: detecta o jogo pelo executável ou por janela em tela cheia, baixa a prioridade do processo, liga o EcoQoS do Windows e coleta dados só a cada 15 s.
- Ícone na bandeja (abrir, widget, modo jogo, sair) e atalho global **Ctrl+Alt+F**.
- Fechar a janela só a esconde; para sair use o botão Sair ou a bandeja.

Consumo medido em release: cerca de 22 MB de RAM e 0,2% de CPU.

## Cérebro e ações (etapa M2)

Digite na caixa **Conversa** da janela principal (a voz da etapa M1 usa o mesmo caminho). Exemplos:

| Pedido | O que acontece |
|---|---|
| "abre o discord", "abre o counter strike" | Procura no Menu Iniciar, na Área de Trabalho, nos jogos da Steam e nos seus apelidos |
| "volume 30", "aumenta o volume", "muta" | Volume geral do Windows |
| "pausa", "próxima", "música anterior" | Teclas de mídia (Spotify, YouTube, etc.) |
| "me lembra de tirar a pizza em 20 minutos", "me avisa amanhã às 9h de pagar a conta" | Lembrete salvo no SQLite; quando vence aparece na conversa e no widget, sem roubar o foco do jogo |
| "modo trabalho" | Roda uma rotina do `config.toml` |
| "fecha o programa que travou", "fecha o chrome" | Pergunta antes e só fecha depois do "sim" |
| "como está meu PC?", "por que está lento?" | O LLM lê as métricas do monitor e responde |
| "eu jogo CS2 toda noite" | O LLM guarda o fato e usa nas próximas conversas |

Comandos simples (volume, mídia, abrir app, lembretes, rotinas, hora) são resolvidos **sem LLM**, na hora e sem rede. O resto vai para o LLM com ferramentas:

- **Groq** (padrão): coloque a chave em `brain.groq_api_key` ou na variável `GROQ_API_KEY`. Não usa nada do PC, ideal enquanto joga.
- **Ollama** (local): `brain.provider = "ollama"` e `ollama pull qwen2.5:7b`. No modo jogo o modelo é descarregado da VRAM depois de cada resposta.

O histórico, os fatos e os lembretes ficam em `%APPDATA%\sexta-feira\data\memoria.db`.

## Compilar

Pré-requisito: [Rust](https://rustup.rs) estável.

```
cargo run --release
```

O executável fica em `target/release/sexta-feira.exe`. Não precisa de nenhuma DLL extra.

Para gerar o `.exe` a partir do Linux:

```
rustup target add x86_64-pc-windows-gnu
sudo apt install gcc-mingw-w64-x86-64
cargo build --release --target x86_64-pc-windows-gnu
```

## Configuração

Na primeira execução é criado `%APPDATA%\sexta-feira\config\config.toml`:

| Chave | Padrão | O que faz |
|---|---|---|
| `monitor.interval_ms` | 2000 | Intervalo de coleta normal |
| `monitor.game_interval_ms` | 15000 | Intervalo de coleta no modo jogo |
| `monitor.temp_alert_c` | 90 | Temperatura de alerta da CPU e da GPU |
| `monitor.ram_alert_pct` | 90 | Uso de RAM que gera alerta |
| `monitor.disk_free_alert_pct` | 10 | Espaço livre mínimo nos discos |
| `game_mode.auto` | true | Liga o modo jogo sozinho |
| `game_mode.detect_fullscreen` | true | Trata qualquer janela em tela cheia como jogo |
| `game_mode.executables` | lista de jogos populares | Executáveis que sempre ligam o modo jogo |
| `game_mode.hide_widget` | false | Esconde o widget durante o jogo |
| `brain.provider` | `"groq"` | `"groq"` ou `"ollama"` |
| `brain.groq_api_key` | vazio | Chave da Groq (vazio = usa `GROQ_API_KEY`) |
| `brain.groq_model` | `openai/gpt-oss-120b` | Qualquer modelo da Groq com suporte a ferramentas |
| `brain.ollama_url` | `http://localhost:11434` | Endereço do Ollama |
| `brain.ollama_model` | `qwen2.5:7b` | Modelo local com suporte a ferramentas |
| `brain.ollama_keep_alive` | `"5m"` | Quanto tempo o modelo fica na memória fora do jogo |
| `brain.ollama_unload_in_game` | true | Tira o modelo da VRAM durante o jogo |
| `brain.history_messages` | 12 | Mensagens anteriores enviadas como contexto |
| `brain.apps` | vazio | Apelidos: `"cs" = "steam://rungameid/730"` |
| `brain.routines` | `trabalho` | Rotinas: lista de passos como `"abrir code"`, `"volume 30"`, `"fechar spotify"`, `"esperar 2"` |

Exemplo:

```toml
[brain]
provider = "groq"
groq_api_key = "gsk_..."

[brain.apps]
cs = "steam://rungameid/730"
"meu servidor" = "C:\\Users\\voce\\servidor.bat"

[brain.routines]
trabalho = ["abrir visual studio code", "abrir chrome", "volume 30"]
jogatina = ["fechar chrome", "abrir discord", "abrir steam", "volume 60"]
```

## Estrutura

| Arquivo | Papel |
|---|---|
| `src/main.rs` | Inicialização, janelas e a thread de monitoramento |
| `src/monitor.rs` | Coleta de métricas com frequências diferentes por tipo de dado |
| `src/gamemode.rs` | Detecção de jogo e economia de energia (EcoQoS) |
| `src/tray.rs` | Bandeja e atalho global (só Windows) |
| `src/config.rs` | Leitura e gravação do `config.toml` |
| `src/brain/` | Cérebro: atalho sem LLM (`quick.rs`), Groq/Ollama (`llm.rs`), ferramentas (`tools.rs`), SQLite (`memory.rs`), lembretes (`reminders.rs`) |
| `src/actions/` | Ações no Windows: apps e jogos, volume, mídia, programas travados |
| `ui/*.slint` | Interface (Slint) |

## Próximas etapas

- **M1 Voz:** palavra de ativação "Sexta-Feira", Whisper local e motor de voz neural próprio (Chatterbox Multilingual, Kokoro no modo jogo).
- **M3 Modo jogo:** medir o impacto real em FPS e ajustar.
- **M4 Diagnóstico:** Log de Eventos do Windows, SMART dos discos e drivers.
