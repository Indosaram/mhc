const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  async fetch(request) {
    if (new URL(request.url).pathname !== "/v1/chat/completions") return new Response("Not found", { status: 404 });
    const body = await request.json() as { model: string; stream: boolean; messages: Array<{ role: string; content: unknown }> };
    const last = body.messages.findLast(message => message.role === "user");
    const text = typeof last?.content === "string" ? last.content : JSON.stringify(last?.content ?? null);
    console.log(JSON.stringify({ receipt: "provider_request", path: "/v1/chat/completions", model: body.model, stream: body.stream, text }));
    if (body.model !== "native" || !body.stream) return Response.json({ error: { message: "unexpected model or non-stream request" } }, { status: 400 });
    if (text.includes("task44-error")) return Response.json({ error: { message: "task44 deterministic provider rejection", type: "invalid_request_error" } }, { status: 400 });
    const cancel = text.includes("task44-cancel") || text.includes("task44-drop");
    const resumed = text.includes("task44-resumed");
    const content = resumed ? "task44-native-resumed" : "task44-native-provider";
    const encoder = new TextEncoder();
    const chunk = (delta: Record<string, string>, finish_reason: string | null) => encoder.encode(`data: ${JSON.stringify({ id: "task44-native-wire", object: "chat.completion.chunk", created: 1, model: "native", choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(chunk({ role: "assistant", content: cancel ? "task44-cancellation-held" : content }, null));
        if (cancel) {
          request.signal.addEventListener("abort", () => {
            console.log(JSON.stringify({ receipt: "provider_abort", text }));
            controller.close();
          }, { once: true });
          return;
        }
        controller.enqueue(chunk({}, "stop"));
        controller.enqueue(encoder.encode("data: [DONE]\n\n"));
        controller.close();
      },
      cancel() { console.log(JSON.stringify({ receipt: "provider_stream_cancel", text })); },
    });
    return new Response(stream, { headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" } });
  },
});
console.log(JSON.stringify({ receipt: "provider_ready", url: `${server.url}v1` }));
process.on("SIGTERM", () => { server.stop(true); process.exit(0); });
process.on("SIGINT", () => { server.stop(true); process.exit(0); });
