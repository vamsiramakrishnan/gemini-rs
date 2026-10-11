"""Minimal Live API stand-in: setupComplete, then for each client turn a short
spoken reply (5 x 40 ms of 24 kHz PCM16, base64) with a transcript, then
turnComplete with usage. Runs in its own process so it never counts toward a
client's memory."""
import asyncio, base64, json, sys
import websockets

AUDIO = base64.b64encode(b"\x00\x01" * 960).decode()  # 40 ms @ 24 kHz mono PCM16

async def handle(ws):
    try:
        await ws.recv()  # setup
        await ws.send(json.dumps({"setupComplete": {}}))
        async for raw in ws:
            msg = json.loads(raw)
            # google-genai sends snake_case keys; the Rust codec camelCase.
            if not ({"clientContent", "client_content"} & msg.keys()):
                continue
            body = msg.get("clientContent") or msg.get("client_content")
            text = body["turns"][0]["parts"][0].get("text", "")
            await asyncio.sleep(0.02)
            if text.startswith("stream:"):
                # Real-time speech: one 40 ms frame every 40 ms for N seconds.
                frames = int(float(text.split(":")[1]) * 25)
                loop = asyncio.get_running_loop()
                start = loop.time()
                for i in range(frames):
                    await ws.send(json.dumps({"serverContent": {"modelTurn": {"parts": [
                        {"inlineData": {"mimeType": "audio/pcm;rate=24000", "data": AUDIO}}]}}}))
                    await asyncio.sleep(max(0.0, start + (i + 1) * 0.04 - loop.time()))
            else:
                for _ in range(5):
                    await ws.send(json.dumps({"serverContent": {"modelTurn": {"parts": [
                        {"inlineData": {"mimeType": "audio/pcm;rate=24000", "data": AUDIO}}]}}}))
            await ws.send(json.dumps({"serverContent": {"outputTranscription": {"text": "Understood."},
                                                         "modelTurn": {"parts": [{"text": "Understood."}]}}}))
            await ws.send(json.dumps({"serverContent": {"turnComplete": True},
                                      "usageMetadata": {"promptTokenCount": 10, "responseTokenCount": 5,
                                                        "totalTokenCount": 15}}))
    except websockets.ConnectionClosed:
        pass

async def main(port):
    async with websockets.serve(handle, "127.0.0.1", port, max_size=None, backlog=4096):
        print("ready", flush=True)
        await asyncio.Future()

asyncio.run(main(int(sys.argv[1])))
