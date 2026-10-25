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

Consumo medido em release: cerca de 22 MB de RAM e 0,2% de CPU.

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
| `diagnostics.auto_interval_h` | 6 | De quantas em quantas horas o diagnóstico roda sozinho (0 = só quando pedir) |
| `diagnostics.startup_delay_s` | 180 | Espera após abrir antes do primeiro diagnóstico automático |
| `diagnostics.event_days` | 7 | Dias do Log de Eventos analisados |
| `diagnostics.old_driver_years` | 3 | Idade a partir da qual drivers de rede, áudio, chipset etc. aparecem |

## Estrutura

| Arquivo | Papel |
|---|---|
| `src/main.rs` | Inicialização, janelas e a thread de monitoramento |
| `src/monitor.rs` | Coleta de métricas com frequências diferentes por tipo de dado |
| `src/gamemode.rs` | Detecção de jogo e economia de energia (EcoQoS) |
| `src/diagnostics/` | Diagnóstico profundo: coleta (`collect/`), regras (`rules.rs`), ferramenta do cérebro (`tool.rs`) |
| `src/tray.rs` | Bandeja e atalho global (só Windows) |
| `src/config.rs` | Leitura e gravação do `config.toml` |
| `ui/*.slint` | Interface (Slint) |

## Próximas etapas

- **M1 Voz:** palavra de ativação "Sexta-Feira", Whisper local e motor de voz neural próprio (Chatterbox Multilingual, Kokoro no modo jogo).
- **M2 Cérebro e ações:** LLM com ferramentas para abrir apps, controlar volume e mídia, lembretes e rotinas.
- **M3 Modo jogo:** medir o impacto real em FPS e ajustar.
