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
- O que você fala vai para o mesmo caminho da caixa **Conversa**: o cérebro responde e a resposta (e os lembretes) saem falados.

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
## Diagnóstico profundo (etapa M4)

O botão **Diagnóstico** abre a página "o que está errado no meu PC". Ela junta:

- **Log de Eventos** (System e Application, últimos 7 dias): telas azuis, desligamentos inesperados, erros de hardware WHEA, setores defeituosos e erros de disco, erros de NTFS, travamentos do driver de vídeo, serviços falhando e programas que fecham sozinhos (com o nome do programa). Ruído conhecido como DCOM 10016 é ignorado.
- **Discos**: saúde do Windows, previsão de falha do SMART, desgaste do SSD, erros não corrigidos, temperatura e espaço livre.
- **Drivers**: dispositivos com alerta amarelo no Gerenciador de Dispositivos, driver de vídeo com mais de 1 ano e drivers de rede, áudio, chipset e USB muito antigos. A idade é só um indício; não há consulta à internet.
- **Temperaturas**: todos os sensores do sistema, todas as GPUs NVIDIA e os discos. O Windows não mostra a temperatura da CPU para programas comuns; se o [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor) estiver aberto, a Sexta-Feira lê todos os sensores dele.

Cada item vem com gravidade e uma sugestão. A coleta roda numa thread em prioridade de segundo plano do Windows (CPU e disco), só quando você pede ou a cada 6 horas, e nunca sozinha durante o modo jogo. Desgaste, erros e SMART dos discos exigem rodar como administrador; sem isso aparecem em "Não verificado".

Para rodar só o diagnóstico e gravar o JSON (o mesmo que o cérebro recebe):

```
sexta-feira.exe --diagnostico diagnostico.json
```

O cérebro usa a ferramenta `diagnosticar_pc` de `src/diagnostics/tool.rs`: `tool::definition()` vai na lista de ferramentas do modelo e `tool::call(&diag, &input)` devolve o resultado.

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
| `diagnostics.auto_interval_h` | 6 | De quantas em quantas horas o diagnóstico roda sozinho (0 = só quando pedir) |
| `diagnostics.startup_delay_s` | 180 | Espera após abrir antes do primeiro diagnóstico automático |
| `diagnostics.event_days` | 7 | Dias do Log de Eventos analisados |
| `diagnostics.old_driver_years` | 3 | Idade a partir da qual drivers de rede, áudio, chipset etc. aparecem |
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
| `src/tray.rs` | Bandeja e atalhos globais (só Windows) |
| `src/voice/` | Microfone, palavra de ativação, transcrição, fala e controle do servidor de voz |
| `voice-server/` | Servidor Python com Whisper, Chatterbox e Kokoro |
| `src/diagnostics/` | Diagnóstico profundo: coleta (`collect/`), regras (`rules.rs`), ferramenta do cérebro (`tool.rs`) |
| `src/config.rs` | Leitura e gravação do `config.toml` |
| `src/brain/` | Cérebro: atalho sem LLM (`quick.rs`), Groq/Ollama (`llm.rs`), ferramentas (`tools.rs`), SQLite (`memory.rs`), lembretes (`reminders.rs`) |
| `src/actions/` | Ações no Windows: apps e jogos, volume, mídia, programas travados |
| `ui/*.slint` | Interface (Slint) |

## Próximas etapas

- Calibrar a palavra de ativação e os modelos de voz num PC real.
- Medir o impacto real em FPS durante jogos e ajustar o modo jogo.
- Integração com Google Agenda para os eventos.
