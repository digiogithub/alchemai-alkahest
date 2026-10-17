"""Conformance checks with the official `anthropic` Python client (ALK-T-0116).

Usage: python anthropic_sdk.py <base-url> <chat-model> <embed-model>

Exits non-zero on the first failed check; prints one line per passed check.
ALKAHEST_CONFORMANCE_TOOLS=0 skips the tool-call assertions (tiny real models whose chat
template has no tool format answer in plain text); the requests are still sent and parsed.
"""

import os
import sys

import anthropic


def check(name, cond, detail=""):
    if not cond:
        print(f"FAIL {name}: {detail}", flush=True)
        sys.exit(1)
    print(f"ok   {name}", flush=True)


def main():
    base, chat_model = sys.argv[1], sys.argv[2]
    client = anthropic.Anthropic(
        base_url=base,
        api_key=os.environ.get("ALKAHEST_API_KEY", "unused"),
        max_retries=0,
    )

    msg = client.messages.create(
        model=chat_model,
        max_tokens=32,
        system="You are terse.",
        messages=[{"role": "user", "content": "Say hi."}],
    )
    check("messages.create type", msg.type == "message" and msg.role == "assistant", msg)
    text = "".join(b.text for b in msg.content if b.type == "text")
    check("messages.create text", text, msg)
    check("messages.create stop", msg.stop_reason in ("end_turn", "max_tokens"), msg)
    check("messages.create usage", msg.usage.input_tokens > 0 and msg.usage.output_tokens > 0, msg)
    check("messages.create model echo", msg.model == chat_model, msg.model)

    with client.messages.stream(
        model=chat_model,
        max_tokens=32,
        messages=[{"role": "user", "content": "Say hi."}],
    ) as stream:
        streamed = "".join(stream.text_stream)
        final = stream.get_final_message()
    check("messages.stream text", streamed, final)
    final_text = "".join(b.text for b in final.content if b.type == "text")
    check("messages.stream final", final_text == streamed, (final_text, streamed))
    check("messages.stream stop", final.stop_reason in ("end_turn", "max_tokens"), final)

    tools = [
        {
            "name": "get_weather",
            "description": "Current weather of a city",
            "input_schema": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            },
        }
    ]
    with client.messages.stream(
        model=chat_model,
        max_tokens=64,
        tools=tools,
        tool_choice={"type": "any"},
        messages=[{"role": "user", "content": "What is the weather in Paris?"}],
    ) as stream:
        tool_msg = stream.get_final_message()
    uses = [b for b in tool_msg.content if b.type == "tool_use"]
    if os.environ.get("ALKAHEST_CONFORMANCE_TOOLS", "1") == "0":
        check("tools stream parses", tool_msg.type == "message", tool_msg)
        print("skip tool-call assertions (ALKAHEST_CONFORMANCE_TOOLS=0)", flush=True)
        uses = None
    else:
        check("tools stream tool_use", uses and uses[0].name == "get_weather", tool_msg)
        check("tools stream input", isinstance(uses[0].input, dict), uses[0])
        check("tools stop_reason", tool_msg.stop_reason == "tool_use", tool_msg)

    use = (
        {"type": "tool_use", "id": uses[0].id, "name": uses[0].name, "input": uses[0].input}
        if uses
        else {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
    )
    follow = client.messages.create(
        model=chat_model,
        max_tokens=32,
        tools=tools,
        messages=[
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "content": [use]},
            {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": use["id"], "content": "sunny, 21 C"}
                ],
            },
        ],
    )
    check("tool_result follow-up", follow.type == "message", follow)

    count = client.messages.count_tokens(
        model=chat_model, messages=[{"role": "user", "content": "Say hi."}]
    )
    check("count_tokens", count.input_tokens > 0, count)

    try:
        client.messages.create(
            model="no-such-model", max_tokens=4, messages=[{"role": "user", "content": "x"}]
        )
        check("unknown model NotFoundError", False, "no error")
    except anthropic.NotFoundError as e:
        check("unknown model NotFoundError", e.body["error"]["type"] == "not_found_error", e.body)

    try:
        client.messages.create(
            model=chat_model, max_tokens=4, messages=[{"role": "system", "content": "x"}]
        )
        check("bad role BadRequestError", False, "no error")
    except anthropic.BadRequestError as e:
        check(
            "bad role BadRequestError",
            e.body["error"]["type"] == "invalid_request_error",
            e.body,
        )


if __name__ == "__main__":
    main()
