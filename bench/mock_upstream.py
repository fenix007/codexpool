#!/usr/bin/env python3
"""
Мок Codex-upstream для самопроверки harness и для нагрузочных тестов роутера без реальных аккаунтов.

Эмулирует POST /v1/responses и /v1/chat/completions со стримом SSE:
  --ttft 300      задержка до первого дельта, мс
  --tps 60        скорость выдачи токенов
  --tokens 80     число токенов в ответе
  --fail-429 0.1  доля запросов, отвечающих 429 (для проверки failover в codexpool)
  --fail-mid 0.05 доля стримов, обрывающихся посередине

  python3 mock_upstream.py --port 9911 --ttft 250 --tps 80
  python3 sse_bench.py --target mock --base-url http://127.0.0.1:9911/v1 --api-key x --model m --repeat 2 --out /tmp/mock.jsonl
"""
import argparse
import json
import random
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ARGS = None
WORDS = "поток токен прокси окно квота аккаунт ответ дельта задержка событие".split()


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        is_resp = self.path.endswith("/responses")
        if random.random() < ARGS.fail_429:
            payload = json.dumps({"error": {"type": "usage_limit_reached", "message": "You have hit your usage limit.",
                                            "resets_in_seconds": 3600}}).encode()
            self.send_response(429)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("x-codex-primary-used-percent", "100")
            self.end_headers()
            self.wfile.write(payload)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("x-codexpool-account", "mock-acct-1")
        self.send_header("Connection", "close")
        self.end_headers()
        time.sleep(ARGS.ttft / 1000)
        n_tok = body.get("max_output_tokens") or body.get("max_tokens") or ARGS.tokens
        n_tok = min(n_tok, ARGS.tokens)
        abort_at = int(n_tok * random.random()) if random.random() < ARGS.fail_mid else None
        for i in range(n_tok):
            if abort_at is not None and i == abort_at:
                self.connection.close()
                return
            w = random.choice(WORDS) + " "
            if is_resp:
                ev = {"type": "response.output_text.delta", "delta": w, "output_index": 0, "content_index": 0}
                self.wfile.write(f"event: response.output_text.delta\ndata: {json.dumps(ev, ensure_ascii=False)}\n\n".encode())
            else:
                ev = {"id": "chatcmpl-mock", "object": "chat.completion.chunk", "model": body.get("model"),
                      "choices": [{"index": 0, "delta": {"content": w}, "finish_reason": None}]}
                self.wfile.write(f"data: {json.dumps(ev, ensure_ascii=False)}\n\n".encode())
            self.wfile.flush()
            time.sleep(1 / ARGS.tps)
        usage = {"input_tokens": 50, "output_tokens": n_tok, "total_tokens": 50 + n_tok}
        if is_resp:
            done = {"type": "response.completed", "response": {"id": "resp_mock", "status": "completed", "usage": usage}}
            self.wfile.write(f"event: response.completed\ndata: {json.dumps(done)}\n\n".encode())
        else:
            usage_ev = {"id": "chatcmpl-mock", "object": "chat.completion.chunk", "choices": [],
                        "usage": {"prompt_tokens": 50, "completion_tokens": n_tok, "total_tokens": 50 + n_tok}}
            self.wfile.write(f"data: {json.dumps(usage_ev)}\n\ndata: [DONE]\n\n".encode())
        self.wfile.flush()


def main():
    global ARGS
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=9911)
    ap.add_argument("--ttft", type=float, default=300)
    ap.add_argument("--tps", type=float, default=60)
    ap.add_argument("--tokens", type=int, default=80)
    ap.add_argument("--fail-429", type=float, default=0.0)
    ap.add_argument("--fail-mid", type=float, default=0.0)
    ARGS = ap.parse_args()
    srv = ThreadingHTTPServer(("127.0.0.1", ARGS.port), H)
    print(f"mock upstream on http://127.0.0.1:{ARGS.port}/v1 ttft={ARGS.ttft}ms tps={ARGS.tps}")
    srv.serve_forever()


if __name__ == "__main__":
    main()
