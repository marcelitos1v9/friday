"""Servidor de voz local da Sexta-Feira.

Roda Whisper (fala -> texto), Chatterbox Multilingual (voz clonada) e Kokoro (voz leve).
Cada modelo só é carregado na primeira vez que é usado e pode ser descarregado por /unload,
para liberar a GPU quando um jogo começa. O app em Rust inicia e encerra este processo.

Endpoints (somente 127.0.0.1):
  GET  /health
  POST /stt     corpo: WAV           -> {"text": "..."}
  POST /tts     corpo: JSON {text, engine, lang, kokoro_voice, voice_sample} -> WAV
  POST /unload  corpo: JSON {engines: ["chatterbox", "whisper", "kokoro"]}

Com FRIDAY_FAKE=1 os modelos são trocados por respostas fixas (para testes).
"""

import argparse
import gc
import io
import json
import os
import threading
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np

FAKE = os.environ.get("FRIDAY_FAKE") == "1"
LOCK = threading.Lock()
MODELS = {}
ARGS = None


def device():
    try:
        import torch

        return "cuda" if torch.cuda.is_available() else "cpu"
    except ImportError:
        return "cpu"


def read_wav(data):
    with wave.open(io.BytesIO(data)) as w:
        frames = w.readframes(w.getnframes())
        width, channels, rate = w.getsampwidth(), w.getnchannels(), w.getframerate()
    if width != 2:
        raise ValueError("WAV precisa ser PCM 16 bits")
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    if channels > 1:
        audio = audio.reshape(-1, channels).mean(axis=1)
    if rate != 16000:
        n = int(len(audio) * 16000 / rate)
        audio = np.interp(np.linspace(0, len(audio) - 1, n), np.arange(len(audio)), audio).astype(np.float32)
    return audio


def write_wav(audio, rate):
    audio = np.clip(np.asarray(audio, dtype=np.float32).reshape(-1), -1.0, 1.0)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(int(rate))
        w.writeframes((audio * 32767).astype(np.int16).tobytes())
    return buf.getvalue()


def whisper():
    if "whisper" not in MODELS:
        from faster_whisper import WhisperModel

        dev = device()
        MODELS["whisper"] = WhisperModel(
            ARGS.stt_model, device=dev, compute_type="float16" if dev == "cuda" else "int8"
        )
    return MODELS["whisper"]


def kokoro():
    if "kokoro" not in MODELS:
        from kokoro import KPipeline

        # 'p' = português do Brasil. Fica na CPU para não disputar a GPU com jogos.
        MODELS["kokoro"] = KPipeline(lang_code="p", device="cpu")
    return MODELS["kokoro"]


def chatterbox():
    if "chatterbox" not in MODELS:
        from chatterbox.mtl_tts import ChatterboxMultilingualTTS

        MODELS["chatterbox"] = ChatterboxMultilingualTTS.from_pretrained(device=device())
    return MODELS["chatterbox"]


def stt(audio):
    if FAKE:
        return "teste de transcrição"
    segments, _ = whisper().transcribe(audio, language="pt", beam_size=1, vad_filter=True)
    return " ".join(s.text.strip() for s in segments).strip()


def tts(req):
    text = (req.get("text") or "").strip()
    engine = req.get("engine", "kokoro")
    if not text:
        raise ValueError("texto vazio")
    if FAKE:
        t = np.linspace(0, 0.5, 12000, endpoint=False)
        return write_wav(0.2 * np.sin(2 * np.pi * 440 * t), 24000)
    if engine == "chatterbox":
        model = chatterbox()
        sample = req.get("voice_sample") or None
        if sample and not os.path.exists(sample):
            sample = None
        wav = model.generate(text, language_id=req.get("lang", "pt"), audio_prompt_path=sample)
        return write_wav(wav.squeeze().detach().cpu().numpy(), model.sr)
    if engine == "kokoro":
        voice = req.get("kokoro_voice") or "pf_dora"
        chunks = [np.asarray(audio) for _, _, audio in kokoro()(text, voice=voice)]
        return write_wav(np.concatenate(chunks) if chunks else np.zeros(1), 24000)
    raise ValueError(f"motor desconhecido: {engine}")


def unload(engines):
    for name in engines:
        MODELS.pop(name, None)
    gc.collect()
    try:
        import torch

        if torch.cuda.is_available():
            torch.cuda.empty_cache()
    except ImportError:
        pass


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def reply(self, status, body, ctype="application/json"):
        if isinstance(body, (dict, list)):
            body = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self.reply(200, {"ok": True, "loaded": sorted(MODELS), "device": device(), "fake": FAKE})
        else:
            self.reply(404, {"error": "rota desconhecida"})

    def do_POST(self):
        data = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        try:
            with LOCK:
                if self.path == "/stt":
                    self.reply(200, {"text": stt(read_wav(data))})
                elif self.path == "/tts":
                    self.reply(200, tts(json.loads(data)), "audio/wav")
                elif self.path == "/unload":
                    unload(json.loads(data or b"{}").get("engines", []))
                    self.reply(200, {"ok": True, "loaded": sorted(MODELS)})
                else:
                    self.reply(404, {"error": "rota desconhecida"})
        except Exception as e:  # o app mostra a mensagem para o usuário
            self.reply(500, {"error": f"{type(e).__name__}: {e}"})


def main():
    global ARGS
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=8765)
    p.add_argument("--stt-model", default="small")
    ARGS = p.parse_args()
    ThreadingHTTPServer(("127.0.0.1", ARGS.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
