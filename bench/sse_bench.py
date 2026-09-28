#!/usr/bin/env python3
"""
codexpool bench — синтетический SSE-клиент для замера TTFT / TPS / error-rate
через любой OpenAI-совместимый бэкенд (прямой OpenAI, OmniRoute, CLIProxyAPI, codexpool).

Только стандартная библиотека (Python 3.9+), чтобы harness запускался где угодно.

Пример:
  python3 sse_bench.py --target omniroute --base-url http://localhost:20128/v1 \
      --api-key $KEY --model gpt-5.1-codex --api responses \
      --corpus corpus.json --repeat 5 --concurrency 2 --out results/omniroute.jsonl

Метрики на запрос (одна JSON-строка в --out):
  ttft_ms          время от отправки запроса до первого content-дельта (не до заголовков!)
  headers_ms       время до получения HTTP-заголовков (отдельно — чтобы видеть overhead прокси)
  total_ms         полное время до закрытия стрима
  output_tokens    из usage в финальном событии (если бэкенд отдаёт), иначе оценка по дельтам
  tps              output_tokens / (total_ms - ttft_ms) * 1000
  status           http-код; 0 = сетевая ошибка/таймаут
  error_class      ok | http_401 | http_429 | http_5xx | http_other | timeout | stream_abort | bad_sse
  account_hint     заголовок x-codexpool-account / x-omniroute-connection, если прокси его выставляет
"""
import argparse
import concurrent.futures as cf
import http.client
import json
import os
import ssl
import sys
import time
import urllib.parse
import uuid


def build_body(api, model, item, stream=True):
    if api == "responses":
        body = {
            "model": model,
            "input": item["prompt"],
            "stream": stream,
            "store": False,
        }
        if item.get("instructions"):
            body["instructions"] = item["instructions"]
        if item.get("tools"):
            body["tools"] = item["tools"]
        if item.get("reasoning_effort"):
            body["reasoning"] = {"effort": item["reasoning_effort"]}
        if item.get("max_output_tokens"):
            body["max_output_tokens"] = item["max_output_tokens"]
        return body
    # chat/completions
    msgs = []
    if item.get("instructions"):
        msgs.append({"role": "system", "content": item["instructions"]})
    msgs.append({"role": "user", "content": item["prompt"]})
    body = {"model": model, "messages": msgs, "stream": stream}
    if stream:
        body["stream_options"] = {"include_usage": True}
    if item.get("max_output_tokens"):
        body["max_tokens"] = item["max_output_tokens"]
    return body


def is_content_delta(api, event_name, data):
    """Первый байт полезного текста — именно его считаем TTFT."""
    if api == "responses":
        t = data.get("type", event_name or "")
        return t in ("response.output_text.delta", "response.reasoning_summary_text.delta",
                     "response.function_call_arguments.delta", "response.output_item.added")
    choices = data.get("choices") or []
    if not choices:
        return False
    delta = choices[0].get("delta") or {}
    return bool(delta.get("content") or delta.get("tool_calls") or delta.get("reasoning_content"))


def is_text_delta(api, data):
    """Первый видимый пользователю текст (без reasoning и tool call)."""
    if api == "responses":
        return data.get("type") == "response.output_text.delta"
    choices = data.get("choices") or []
    return bool(choices and (choices[0].get("delta") or {}).get("content"))


def extract_usage(api, data):
    if api == "responses":
        if data.get("type") == "response.completed":
            return (data.get("response") or {}).get("usage")
        return None
    return data.get("usage")


def one_request(args, item, run_idx):
    url = urllib.parse.urlsplit(args.base_url)
    path = url.path.rstrip("/") + ("/responses" if args.api == "responses" else "/chat/completions")
    body = json.dumps(build_body(args.api, args.model, item)).encode()
    headers = {
        "Authorization": f"Bearer {args.api_key}",
        "Content-Type": "application/json",
        "Accept": "text/event-stream",
        "User-Agent": "codexpool-bench/0.1",
        "x-bench-run-id": args.run_id,
        "x-bench-case": f"{item['id']}#{run_idx}",
    }
    rec = {
        "run_id": args.run_id, "target": args.target, "api": args.api, "model": args.model,
        "case": item["id"], "run": run_idx, "ts": time.time(),
        "status": 0, "error_class": "ok", "ttft_ms": None, "headers_ms": None, "total_ms": None,
        "output_tokens": None, "input_tokens": None, "tps": None, "deltas": 0, "bytes": 0,
        "ttft_text_ms": None, "first_event_bytes": None,
        "account_hint": None, "error": None,
    }
    conn_cls = http.client.HTTPSConnection if url.scheme == "https" else http.client.HTTPConnection
    kw = {"timeout": args.timeout}
    if url.scheme == "https" and args.insecure:
        kw["context"] = ssl._create_unverified_context()
    t0 = time.perf_counter()
    conn = conn_cls(url.hostname, url.port, **kw)
    try:
        conn.request("POST", path, body=body, headers=headers)
        resp = conn.getresponse()
        rec["headers_ms"] = round((time.perf_counter() - t0) * 1000, 1)
        rec["status"] = resp.status
        rec["account_hint"] = (resp.getheader("x-codexpool-account")
                               or resp.getheader("x-omniroute-connection")
                               or resp.getheader("x-account-id"))
        if resp.status != 200:
            payload = resp.read(4096).decode("utf-8", "replace")
            rec["error"] = payload[:500]
            rec["error_class"] = classify_http(resp.status)
            rec["total_ms"] = round((time.perf_counter() - t0) * 1000, 1)
            return rec
        buf = b""
        event_name = None
        first_delta_at = None
        est_tokens = 0
        terminal = False
        while not terminal:
            chunk = resp.read1(65536) if hasattr(resp, "read1") else resp.read(65536)
            if not chunk:
                break
            rec["bytes"] += len(chunk)
            buf += chunk
            while b"\n\n" in buf:
                frame, buf = buf.split(b"\n\n", 1)
                data_lines = []
                for line in frame.split(b"\n"):
                    if line.startswith(b"event:"):
                        event_name = line[6:].strip().decode()
                    elif line.startswith(b"data:"):
                        data_lines.append(line[5:].strip())
                if not data_lines:
                    continue
                raw = b"\n".join(data_lines)
                if rec["first_event_bytes"] is None:
                    rec["first_event_bytes"] = len(frame)
                if raw == b"[DONE]":
                    terminal = True
                    continue
                try:
                    data = json.loads(raw)
                except json.JSONDecodeError:
                    rec["error_class"] = "bad_sse"
                    rec["error"] = raw[:200].decode("utf-8", "replace")
                    continue
                if data.get("error") or data.get("type") == "error":
                    rec["error_class"] = "stream_error"
                    rec["error"] = json.dumps(data)[:500]
                if is_content_delta(args.api, event_name, data):
                    rec["deltas"] += 1
                    est_tokens += 1
                    if first_delta_at is None:
                        first_delta_at = time.perf_counter()
                        rec["ttft_ms"] = round((first_delta_at - t0) * 1000, 1)
                if data.get("type") in ("response.completed", "response.failed", "response.incomplete"):
                    terminal = True
                if args.api == "chat" and (data.get("choices") or [{}])[0].get("finish_reason") and not data.get("usage"):
                    pass  # ждём usage-чанк или [DONE]
                if rec["ttft_text_ms"] is None and is_text_delta(args.api, data):
                    rec["ttft_text_ms"] = round((time.perf_counter() - t0) * 1000, 1)
                usage = extract_usage(args.api, data)
                if usage:
                    rec["output_tokens"] = usage.get("output_tokens", usage.get("completion_tokens"))
                    rec["input_tokens"] = usage.get("input_tokens", usage.get("prompt_tokens"))
        t_end = time.perf_counter()
        rec["total_ms"] = round((t_end - t0) * 1000, 1)
        if rec["ttft_ms"] is None and rec["error_class"] == "ok":
            rec["error_class"] = "stream_abort"
        if rec["output_tokens"] is None and est_tokens:
            rec["output_tokens"] = est_tokens
            rec["tokens_estimated"] = True
        if rec["ttft_ms"] is not None and rec["output_tokens"]:
            gen_ms = max(rec["total_ms"] - rec["ttft_ms"], 1.0)
            rec["tps"] = round(rec["output_tokens"] / gen_ms * 1000, 2)
    except (TimeoutError, OSError) as e:  # socket.timeout наследует OSError
        rec["error_class"] = "timeout" if "timed out" in str(e).lower() else "net_error"
        rec["error"] = repr(e)
        rec["total_ms"] = round((time.perf_counter() - t0) * 1000, 1)
    except Exception as e:  # noqa
        rec["error_class"] = "client_exception"
        rec["error"] = repr(e)
        rec["total_ms"] = round((time.perf_counter() - t0) * 1000, 1)
    finally:
        conn.close()
    return rec


def classify_http(status):
    if status == 401:
        return "http_401"
    if status == 429:
        return "http_429"
    if 500 <= status < 600:
        return "http_5xx"
    return "http_other"


def percentile(vals, p):
    if not vals:
        return None
    vals = sorted(vals)
    k = (len(vals) - 1) * p
    f, c = int(k), min(int(k) + 1, len(vals) - 1)
    return round(vals[f] + (vals[c] - vals[f]) * (k - f), 1)


def summarize(records):
    ok = [r for r in records if r["error_class"] == "ok"]
    ttft = [r["ttft_ms"] for r in ok if r["ttft_ms"] is not None]
    hdr = [r["headers_ms"] for r in ok if r["headers_ms"] is not None]
    tps = [r["tps"] for r in ok if r["tps"]]
    total = [r["total_ms"] for r in ok]
    classes = {}
    for r in records:
        classes[r["error_class"]] = classes.get(r["error_class"], 0) + 1
    return {
        "n": len(records), "ok": len(ok),
        "error_rate": round(1 - len(ok) / len(records), 4) if records else None,
        "error_classes": classes,
        "ttft_ms": {"p50": percentile(ttft, .5), "p95": percentile(ttft, .95), "min": min(ttft) if ttft else None},
        "ttft_text_ms": {"p50": percentile([r["ttft_text_ms"] for r in ok if r.get("ttft_text_ms") is not None], .5),
                         "p95": percentile([r["ttft_text_ms"] for r in ok if r.get("ttft_text_ms") is not None], .95)},
        "first_event_bytes_p50": percentile([r["first_event_bytes"] for r in ok if r.get("first_event_bytes")], .5),
        "headers_ms": {"p50": percentile(hdr, .5), "p95": percentile(hdr, .95)},
        "tps": {"p50": percentile(tps, .5), "p95": percentile(tps, .95)},
        "total_ms": {"p50": percentile(total, .5), "p95": percentile(total, .95)},
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--target", required=True, help="метка бэкенда: direct | omniroute | cliproxyapi | codexpool")
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--api-key", default=os.environ.get("BENCH_API_KEY", ""))
    ap.add_argument("--model", required=True)
    ap.add_argument("--api", choices=["responses", "chat"], default="responses")
    ap.add_argument("--corpus", default=os.path.join(os.path.dirname(__file__), "corpus.json"))
    ap.add_argument("--cases", help="список id кейсов через запятую (по умолчанию все)")
    ap.add_argument("--repeat", type=int, default=3)
    ap.add_argument("--concurrency", type=int, default=1)
    ap.add_argument("--warmup", type=int, default=1, help="прогревочных запросов (не учитываются)")
    ap.add_argument("--timeout", type=float, default=120)
    ap.add_argument("--insecure", action="store_true")
    ap.add_argument("--out", required=True, help="JSONL с записями по каждому запросу")
    ap.add_argument("--run-id", default=None)
    args = ap.parse_args()
    args.run_id = args.run_id or f"{args.target}-{time.strftime('%Y%m%d-%H%M%S')}-{uuid.uuid4().hex[:6]}"

    with open(args.corpus, encoding="utf-8") as f:
        corpus = json.load(f)["cases"]
    if args.cases:
        want = set(args.cases.split(","))
        corpus = [c for c in corpus if c["id"] in want]

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)

    for i in range(args.warmup):
        r = one_request(args, corpus[0], -1 - i)
        print(f"[warmup] {r['error_class']} ttft={r['ttft_ms']}ms", file=sys.stderr)

    jobs = [(c, i) for i in range(args.repeat) for c in corpus]
    records = []
    with open(args.out, "a", encoding="utf-8") as out, cf.ThreadPoolExecutor(args.concurrency) as pool:
        for rec in pool.map(lambda j: one_request(args, j[0], j[1]), jobs):
            records.append(rec)
            out.write(json.dumps(rec, ensure_ascii=False) + "\n")
            out.flush()
            print(f"{rec['case']:>14}#{rec['run']} {rec['error_class']:<12} status={rec['status']} "
                  f"hdr={rec['headers_ms']} ttft={rec['ttft_ms']} tps={rec['tps']} acct={rec['account_hint']}",
                  file=sys.stderr)

    summary = summarize(records)
    summary.update({"run_id": args.run_id, "target": args.target, "model": args.model, "api": args.api})
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    with open(args.out.replace(".jsonl", "") + ".summary.json", "w", encoding="utf-8") as f:
        json.dump(summary, f, ensure_ascii=False, indent=2)


if __name__ == "__main__":
    main()
