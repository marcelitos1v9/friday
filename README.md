# Sexta-Feira

Assistente pessoal para Windows inspirado na Sexta-Feira do Homem de Ferro. É um app nativo em Rust, sem navegador embutido, feito para ficar ligado o tempo todo sem atrapalhar os jogos.

## O que já funciona

### Base (M0)

- Janela principal estilo HUD com CPU, memória, GPU NVIDIA (uso, temperatura e VRAM) e o consumo da própria Sexta-Feira.
- Widget flutuante sem borda e sempre no topo. Arraste para mover; clique duplo abre a janela principal. A posição fica salva.
- Diagnóstico: alerta de temperatura alta, RAM quase cheia e disco com pouco espaço.
- Lista dos programas que mais usam memória, agrupados por nome.
- Modo jogo automático: detecta o jogo pelo executável ou por janela em tela cheia, baixa a prioridade do processo, liga o EcoQoS do Windows e coleta dados só a cada 15 s.
- Ícone na bandeja (abrir, widget, modo jogo, sair) e atalho global **Ctrl+Alt+F**.
- Fechar a janela só a esconde; para sair use o botão Sair ou a bandeja.

Consumo medido em release: cerca de 25 MB de RAM e menos de 1% de CPU.

### Voz (M1)

- **Falar com ela:** diga "Sexta-Feira" ou aperte **Ctrl+Alt+Espaço** (funciona dentro de jogos). O botão **Falar** faz o mesmo. Ela grava até você parar de falar.
- **Palavra de ativação treinada por você:** clique em **Treinar voz** e diga "Sexta-Feira" três vezes. O detector é feito em Rust puro (MFCC + DTW) e só faz contas quando há voz no microfone.
- **Fala para texto:** Whisper da Groq (`stt = "groq"`, não usa nada do PC; precisa de `GROQ_API_KEY`) ou Whisper local (`stt = "local"`).
- **Texto para fala:** motor de voz neural próprio, rodando no seu PC: Chatterbox Multilingual (pode clonar uma voz a partir de um WAV de 5 a 15 s) e Kokoro (leve, roda na CPU). Ela começa a falar frase por frase, sem esperar o texto todo.
- Os modelos ficam num servidor Python separado (`voice-server/`), ligado só quando precisa e desligado depois de 5 minutos sem uso.
- Por enquanto ela repete o que entendeu ("Você disse: ..."). O cérebro com Groq/Ollama (M2) vai responder de verdade.

### Modo jogo (M3)

- Desliga a escuta contínua: o microfone fica fechado e só abre com **Ctrl+Alt+Espaço**.
- Descarrega o Chatterbox e o Whisper local da GPU; a voz passa para o Kokoro na CPU.
- O widget vira um overlay de uma linha (CPU e GPU).
- O processo do servidor de voz roda com prioridade abaixo do normal.

## Instalar a voz

1. Baixe o pacote do Windows (aba Actions, artefato `sexta-feira-windows`). Ele traz o `sexta-feira.exe` e a pasta `voice-server`.
2. Na pasta `voice-server`, rode `powershell -ExecutionPolicy Bypass -File install.ps1`. Ele instala Python, espeak-ng, PyTorch (com CUDA se você tiver NVIDIA) e os modelos. Na primeira fala os modelos são baixados (alguns GB).
3. Para usar o Whisper da Groq, crie uma chave em console.groq.com e defina a variável de ambiente `GROQ_API_KEY` (ou `groq_api_key` no config).
4. Para clonar uma voz, aponte `voice_sample` para um WAV limpo de 5 a 15 s. Use só vozes que você tem direito de usar.

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
| `voice.enabled` | true | Liga o microfone e a voz |
| `voice.stt` | "groq" | "groq" (nuvem) ou "local" (servidor de voz) |
| `voice.groq_api_key` | "" | Chave da Groq; vazio usa `GROQ_API_KEY` |
| `voice.tts_engine` | "chatterbox" | Voz fora de jogo: "chatterbox", "kokoro" ou "off" |
| `voice.tts_engine_game` | "kokoro" | Voz durante o modo jogo |
| `voice.voice_sample` | "" | WAV de referência para o Chatterbox clonar |
| `voice.kokoro_voice` | "pf_dora" | Voz do Kokoro (pf_dora, pm_alex, pm_santa) |
| `voice.local_stt_model` | "small" | Modelo do Whisper local |
| `voice.wake_word` | true | Escuta a palavra "Sexta-Feira" |
| `voice.wake_threshold` | 0.5 | Rigor da palavra de ativação (maior = menos falsos positivos) |
| `voice.python` | "python" | Python do servidor; usa `voice-server/.venv` se existir |
| `voice.idle_unload_secs` | 300 | Desliga o servidor de voz depois desse tempo sem uso |

## Estrutura

| Arquivo | Papel |
|---|---|
| `src/main.rs` | Inicialização, janelas e a thread de monitoramento |
| `src/monitor.rs` | Coleta de métricas com frequências diferentes por tipo de dado |
| `src/gamemode.rs` | Detecção de jogo e economia de energia (EcoQoS) |
| `src/tray.rs` | Bandeja e atalhos globais (só Windows) |
| `src/voice/` | Microfone, palavra de ativação, transcrição, fala e controle do servidor de voz |
| `voice-server/` | Servidor Python com Whisper, Chatterbox e Kokoro |
| `src/config.rs` | Leitura e gravação do `config.toml` |
| `ui/*.slint` | Interface (Slint) |

## Próximas etapas

- **M2 Cérebro e ações:** LLM com ferramentas para abrir apps, controlar volume e mídia, lembretes e rotinas.
- **M4 Diagnóstico:** Log de Eventos do Windows, SMART dos discos e drivers.
