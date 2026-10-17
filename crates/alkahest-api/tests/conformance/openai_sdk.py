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

    responses_checks(client, chat_model)
    print("openai-python conformance: all checks passed", flush=True)


def responses_checks(client, chat_model):
    """The Responses API (ALK-US-0036)."""
    r = client.responses.create(
        model=chat_model, input="Say hi.", instructions="Be brief.", max_output_tokens=32
    )
    check("responses.create", r.object == "response" and r.output_text, r)
    check("responses.create status", r.status in ("completed", "incomplete"), r.status)
    check("responses.create usage", r.usage is not None and r.usage.input_tokens > 0, r.usage)

    with client.responses.stream(model=chat_model, input="Say hi.", max_output_tokens=32) as s:
        deltas = [e.delta for e in s if e.type == "response.output_text.delta"]
        final = s.get_final_response()
    check("responses.stream deltas", "".join(deltas), deltas)
    check("responses.stream final", final.output_text == "".join(deltas), final)

    follow = client.responses.create(
        model=chat_model, previous_response_id=r.id, input="Again.", max_output_tokens=16
    )
    check("responses previous_response_id", follow.previous_response_id == r.id, follow)
    got = client.responses.retrieve(r.id)
    check("responses.retrieve", got.id == r.id, got)

    tools = [
        {
            "type": "function",
            "name": "get_weather",
            "description": "Current weather of a city",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            },
        }
    ]
    t = client.responses.create(
        model=chat_model,
        input="What is the weather in Paris?",
        tools=tools,
        tool_choice="required",
        max_output_tokens=64,
    )
    calls = [o for o in t.output if o.type == "function_call"]
    if os.environ.get("ALKAHEST_CONFORMANCE_TOOLS", "1") == "0":
        check("responses tools parse", t.object == "response", t)
    else:
        check("responses function_call", calls and calls[0].name == "get_weather", t)
        out = client.responses.create(
            model=chat_model,
            previous_response_id=t.id,
            input=[{"type": "function_call_output", "call_id": calls[0].call_id, "output": "sunny"}],
            max_output_tokens=32,
        )
        check("responses function_call_output", out.object == "response", out)

    try:
        client.responses.create(model=chat_model, previous_response_id="resp_nope", input="x")
        check("responses unknown previous raises", False, "no error")
    except openai.BadRequestError as e:
        check("responses unknown previous", e.status_code == 400, e.body)


if __name__ == "__main__":
    main()
