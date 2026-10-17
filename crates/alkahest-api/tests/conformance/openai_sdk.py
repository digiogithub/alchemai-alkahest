"""Conformance checks with the official `openai` Python client (ALK-T-0111).

Usage: python openai_sdk.py <base-url> <chat-model> <embed-model>

Exits non-zero on the first failed check; prints one line per passed check.
"""

import os
import sys

import openai


def check(name, cond, detail=""):
    if not cond:
        print(f"FAIL {name}: {detail}", flush=True)
        sys.exit(1)
    print(f"ok   {name}", flush=True)


def main():
    base, chat_model, embed_model = sys.argv[1], sys.argv[2], sys.argv[3]
    client = openai.OpenAI(
        base_url=f"{base}/v1",
        api_key=os.environ.get("ALKAHEST_API_KEY", "unused"),
        max_retries=0,
    )

    models = [m.id for m in client.models.list()]
    check("models.list", any(m.startswith(chat_model) for m in models), models)
    one = client.models.retrieve(models[0])
    check("models.retrieve", one.id == models[0], one)

    resp = client.chat.completions.create(
        model=chat_model,
        messages=[{"role": "user", "content": "Say hi."}],
    )
    choice = resp.choices[0]
    check("chat", choice.message.role == "assistant" and choice.message.content, resp)
    check("chat finish", choice.finish_reason in ("stop", "length"), resp)
    check("chat usage", resp.usage is not None and resp.usage.total_tokens > 0, resp)
    check("chat model echo", resp.model == chat_model, resp.model)

    chunks = list(
        client.chat.completions.create(
            model=chat_model,
            messages=[{"role": "user", "content": "Say hi."}],
            stream=True,
            stream_options={"include_usage": True},
        )
    )
    text = "".join(c.choices[0].delta.content or "" for c in chunks if c.choices)
    check("chat stream", text, chunks)
    check("chat stream usage", chunks[-1].usage is not None, chunks[-1])

    comp = client.completions.create(model=chat_model, prompt="Once upon a time", max_tokens=4)
    check("completions", comp.choices[0].text, comp)

    # The SDK asks for base64 by default and decodes it.
    emb = client.embeddings.create(model=embed_model, input=["a", "b"])
    check("embeddings", len(emb.data) == 2 and len(emb.data[0].embedding) > 0, emb)
    emb = client.embeddings.create(model=embed_model, input="a", encoding_format="float")
    check("embeddings float", len(emb.data[0].embedding) > 0, emb)

    try:
        client.chat.completions.create(
            model="no-such-model", messages=[{"role": "user", "content": "x"}]
        )
        check("unknown model raises", False, "no error")
    except openai.NotFoundError as e:
        check("unknown model status", e.status_code == 404, e.status_code)
        check("unknown model code", e.code == "model_not_found", e.body)
        check("unknown model type", e.type == "invalid_request_error", e.body)

    try:
        client.chat.completions.create(
            model=chat_model, n=2, messages=[{"role": "user", "content": "x"}]
        )
        check("n=2 raises", False, "no error")
    except openai.BadRequestError as e:
        check("n=2 status", e.status_code == 400, e.status_code)
        check("n=2 message", "n = 1" in str(e.message), e.message)

    try:
        client.get("/no-such-route", cast_to=object)
        check("unknown route raises", False, "no error")
    except openai.NotFoundError as e:
        check("unknown route status", e.status_code == 404, e.status_code)

    print("openai-python conformance: all checks passed", flush=True)


if __name__ == "__main__":
    main()
