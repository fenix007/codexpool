#!/usr/bin/env python3
"""
codexpool bench — прогон реальных клиентов (Codex CLI и Claude Code) через выбранный бэкенд.

Меряем то, что видит пользователь: время до первого символа вывода (proxy TTFT + клиентский overhead),
полное время задачи, код завершения, наличие ошибок 429/401 в stderr. Внутренние метрики прокси
(какой аккаунт, сколько ретраев) поднимаем отдельно из Stats API codexpool по x-bench-run-id.

Как направляем клиентов на бэкенд:
  Codex CLI  -> через профиль в ~/.codex/config.toml (model_provider с base_url) + env CODEX_HOME,
                чтобы не трогать основной конфиг пользователя. Используем `codex exec --json`.
  Claude Code-> ANTHROPIC_BASE_URL=<backend>/  ANTHROPIC_AUTH_TOKEN=<key>  и `claude -p --output-format stream-json`.
                Бэкенд должен уметь /v1/messages (трансляция Anthropic -> Responses).

Пример:
  python3 client_bench.py --client codex --target codexpool --base-url http://127.0.0.1:8787/v1 \
      --api-key $KEY --model gpt-5.1-codex --repeat 3 --out results/codex-cli-codexpool.jsonl
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

TASKS = [
    {"id": "cli_short", "prompt": "Ответь одним словом: столица Франции?"},
    {"id": "cli_code", "prompt": "Напиши функцию на Python для n-го числа Фибоначчи итеративно. Только код."},
    {"id": "cli_repo", "prompt": "Перечисли файлы в текущем каталоге и кратко скажи, что это за проект. Ничего не меняй."},
]


def prep_codex_home(base_url, api_key, model):
    home = tempfile.mkdtemp(prefix="codexpool-bench-home-")
    cfg = f'''
model = "{model}"
model_provider = "bench"
approval_policy = "never"
sandbox_mode = "read-only"

[model_providers.bench]
name = "bench backend"
base_url = "{base_url}"
env_key = "BENCH_API_KEY"
wire_api = "responses"
'''
    with open(os.path.join(home, "config.toml"), "w") as f:
        f.write(cfg)
    return home


def run_codex(task, args, run_idx):
    env = dict(os.environ, CODEX_HOME=args.codex_home, BENCH_API_KEY=args.api_key)
    cmd = ["codex", "exec", "--json", "--skip-git-repo-check", task["prompt"]]
    return run_streaming(cmd, env, args, task, run_idx, first_output_marker=None)


def run_claude(task, args, run_idx):
    env = dict(os.environ,
               ANTHROPIC_BASE_URL=args.base_url.rstrip("/").removesuffix("/v1"),
               ANTHROPIC_AUTH_TOKEN=args.api_key,
               ANTHROPIC_MODEL=args.model,
               CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")
    env.pop("ANTHROPIC_API_KEY", None)
    cmd = ["claude", "-p", task["prompt"], "--output-format", "stream-json", "--verbose",
           "--model", args.model, "--permission-mode", "plan"]
    return run_streaming(cmd, env, args, task, run_idx, first_output_marker=b'"type":"assistant"')


def run_streaming(cmd, env, args, task, run_idx, first_output_marker):
    rec = {"run_id": args.run_id, "client": args.client, "target": args.target, "model": args.model,
           "case": task["id"], "run": run_idx, "ts": time.time(),
           "first_output_ms": None, "total_ms": None, "exit_code": None,
           "error_class": "ok", "stderr_tail": None, "stdout_bytes": 0}
    t0 = time.perf_counter()
    try:
        p = subprocess.Popen(cmd, env=env, cwd=args.workdir, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        first = None
        out_chunks = []
        while True:
            line = p.stdout.readline()
            if not line:
                break
            out_chunks.append(line)
            if first is None and (first_output_marker is None or first_output_marker in line):
                first = time.perf_counter()
                rec["first_output_ms"] = round((first - t0) * 1000, 1)
        try:
            p.wait(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            p.kill()
            rec["error_class"] = "timeout"
        err = p.stderr.read().decode("utf-8", "replace")
        rec["exit_code"] = p.returncode
        rec["stdout_bytes"] = sum(len(c) for c in out_chunks)
        rec["stderr_tail"] = err[-800:] if err else None
        low = err.lower()
        if rec["error_class"] == "ok":
            if p.returncode != 0:
                rec["error_class"] = "nonzero_exit"
            if "429" in low or "rate limit" in low or "usage_limit" in low:
                rec["error_class"] = "http_429"
            elif "401" in low or "unauthorized" in low:
                rec["error_class"] = "http_401"
    except FileNotFoundError:
        rec["error_class"] = "client_missing"
        rec["stderr_tail"] = f"{cmd[0]} not found in PATH"
    rec["total_ms"] = round((time.perf_counter() - t0) * 1000, 1)
    return rec


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--client", choices=["codex", "claude"], required=True)
    ap.add_argument("--target", required=True)
    ap.add_argument("--base-url", required=True, help="OpenAI-совместимый /v1 бэкенда")
    ap.add_argument("--api-key", default=os.environ.get("BENCH_API_KEY", ""))
    ap.add_argument("--model", required=True)
    ap.add_argument("--repeat", type=int, default=3)
    ap.add_argument("--timeout", type=float, default=300)
    ap.add_argument("--workdir", default=os.getcwd())
    ap.add_argument("--out", required=True)
    ap.add_argument("--run-id", default=None)
    args = ap.parse_args()
    args.run_id = args.run_id or f"{args.client}-{args.target}-{time.strftime('%Y%m%d-%H%M%S')}-{uuid.uuid4().hex[:6]}"

    if shutil.which(args.client) is None:
        print(f"клиент {args.client} не найден в PATH", file=sys.stderr)
        sys.exit(2)
    if args.client == "codex":
        args.codex_home = prep_codex_home(args.base_url, args.api_key, args.model)
    runner = run_codex if args.client == "codex" else run_claude

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    records = []
    with open(args.out, "a", encoding="utf-8") as out:
        for i in range(args.repeat):
            for task in TASKS:
                rec = runner(task, args, i)
                records.append(rec)
                out.write(json.dumps(rec, ensure_ascii=False) + "\n")
                out.flush()
                print(f"{rec['case']:>10}#{i} {rec['error_class']:<12} first={rec['first_output_ms']} "
                      f"total={rec['total_ms']} exit={rec['exit_code']}", file=sys.stderr)

    ok = [r for r in records if r["error_class"] == "ok"]
    fo = sorted(r["first_output_ms"] for r in ok if r["first_output_ms"] is not None)
    summary = {
        "run_id": args.run_id, "client": args.client, "target": args.target, "n": len(records), "ok": len(ok),
        "error_rate": round(1 - len(ok) / len(records), 4) if records else None,
        "first_output_ms_p50": fo[len(fo) // 2] if fo else None,
        "total_ms_p50": sorted(r["total_ms"] for r in ok)[len(ok) // 2] if ok else None,
    }
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
