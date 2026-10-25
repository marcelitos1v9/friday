# Instala o servidor de voz da Sexta-Feira (Windows).
# Uso: abra o PowerShell nesta pasta e rode:  powershell -ExecutionPolicy Bypass -File install.ps1
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if (-not (Get-Command python -ErrorAction SilentlyContinue)) {
    Write-Host "Instalando Python 3.11..."
    winget install -e --id Python.Python.3.11 --accept-source-agreements --accept-package-agreements
    Write-Host "Feche e abra o PowerShell de novo e rode este script outra vez."
    exit 1
}

# O Kokoro usa o espeak-ng para pronunciar português.
if (-not (Test-Path "C:\Program Files\eSpeak NG")) {
    Write-Host "Instalando espeak-ng..."
    winget install -e --id eSpeak-NG.eSpeak-NG --accept-source-agreements --accept-package-agreements
}

python -m venv .venv
$py = Join-Path $PSScriptRoot ".venv\Scripts\python.exe"
& $py -m pip install --upgrade pip

# PyTorch com CUDA se houver placa NVIDIA; senão, versão para CPU.
if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) {
    & $py -m pip install torch torchaudio --index-url https://download.pytorch.org/whl/cu124
} else {
    & $py -m pip install torch torchaudio --index-url https://download.pytorch.org/whl/cpu
}
& $py -m pip install -r requirements.txt

Write-Host ""
Write-Host "Pronto. Deixe esta pasta (voice-server) ao lado do sexta-feira.exe; o app encontra o ambiente sozinho."
