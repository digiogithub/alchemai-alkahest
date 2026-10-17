"""Conformance checks with the official `ollama` Python client (ALK-T-0107).

Usage: python ollama_sdk.py <base-url> <chat-model> <embed-model>

Exits non-zero on the first failed check; prints one line per passed check.
"""

import sys

import ollama


def check(name, cond, detail=""):
    if not cond:
        print(f"FAIL {name}: {detail}", flush=True)
        sys.exit(1)
    print(f"ok   {name}", flush=True)


def main():
    base, chat_model, embed_model = sys.argv[1], sys.argv[2], sys.argv[3]
    client = ollama.Client(host=base)

    listed = client.list()
    names = [m.model for m in listed.models]
    check("list", any(n.startswith(chat_model) for n in names), names)

    shown = client.show(chat_model)
    check("show", shown.details is not None and shown.modelfile, shown)

    resp = client.chat(
        model=chat_model,
        messages=[{"role": "user", "content": "Say hi."}],
        stream=False,
    )
    check("chat", resp.message.role == "assistant" and resp.message.content, resp)
    check("chat done", resp.done and resp.done_reason in ("stop", "length"), resp)
    check("chat stats", (resp.eval_count or 0) > 0 and (resp.total_duration or 0) > 0, resp)

    parts = list(
        client.chat(
            model=chat_model,
            messages=[{"role": "user", "content": "Say hi."}],
            stream=True,
        )
    )
    text = "".join(p.message.content or "" for p in parts)
    check("chat stream", len(parts) >= 2 and parts[-1].done and text, parts)

    gen = client.generate(model=chat_model, prompt="Once upon a time", stream=False)
    check("generate", gen.response and gen.done, gen)

    emb = client.embed(model=embed_model, input=["a", "b"])
    check("embed", len(emb.embeddings) == 2 and len(emb.embeddings[0]) > 0, emb)

    running = client.ps()
    check("ps", any(m.model.startswith(chat_model) for m in running.models), running)

    try:
        client.chat(model="no-such-model", messages=[{"role": "user", "content": "x"}])
        check("unknown model raises", False, "no error")
    except ollama.ResponseError as e:
        check("unknown model status", e.status_code == 404, e.status_code)
        check("unknown model message", "not found" in e.error, e.error)

    try:
        client.generate(model=chat_model, prompt="x", options={"num_ctx": "big"})
        check("bad option raises", False, "no error")
    except ollama.ResponseError as e:
        check("bad option status", e.status_code == 400, e.status_code)

    print("ollama-python conformance: all checks passed", flush=True)


if __name__ == "__main__":
    main()
