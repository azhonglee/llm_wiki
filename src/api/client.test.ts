import { describe, expect, it, vi } from "vitest";
import { ApiError, consumeSse, createApiClient } from "./client";

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("API client", () => {
  it("uses cookie credentials and obtains CSRF before a write", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({ csrfToken: "csrf-1" }))
      .mockResolvedValueOnce(jsonResponse({ id: "p1", name: "Wiki" }));
    const client = createApiClient({ fetch: fetchMock, baseUrl: "/api/v2" });

    await client.projects.create({ name: "Wiki" });

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/v2/auth/csrf");
    expect(fetchMock.mock.calls[0][1]).toMatchObject({
      credentials: "include",
      method: "POST",
    });
    expect(fetchMock.mock.calls[1][0]).toBe("/api/v2/projects");
    expect(fetchMock.mock.calls[1][1]).toMatchObject({
      credentials: "include",
      method: "POST",
    });
    expect(
      new Headers(fetchMock.mock.calls[1][1]?.headers).get("X-CSRF-Token"),
    ).toBe("csrf-1");
  });

  it("normalizes the documented error envelope", async () => {
    const client = createApiClient({
      fetch: vi.fn<typeof fetch>().mockResolvedValue(
        jsonResponse(
          {
            error: {
              code: "FILE_REVISION_CONFLICT",
              message: "File changed",
              requestId: "req-1",
            },
          },
          409,
        ),
      ),
    });

    await expect(
      client.files.save("p1", "wiki/a.md", {
        content: "next",
        revision: "old",
      }),
    ).rejects.toMatchObject({
      name: "ApiError",
      status: 409,
      code: "FILE_REVISION_CONFLICT",
      requestId: "req-1",
    } satisfies Partial<ApiError>);
  });

  it("uses the server file revision, multipart, and asset URL contracts", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({ csrfToken: "csrf-1" }))
      .mockResolvedValueOnce(
        jsonResponse({ path: "wiki/a.md", content: "next", revision: "new" }),
      )
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(
        jsonResponse(
          {
            items: [{ path: "raw/assets/a.txt", revision: "upload", size: 5 }],
          },
          201,
        ),
      );
    const client = createApiClient({ fetch: fetchMock, baseUrl: "/api/v2" });

    await client.files.save("p1", "wiki/a.md", {
      content: "next",
      revision: "old",
    });
    await client.files.remove("p1", "wiki/a.md", "new");
    await client.files.upload(
      "p1",
      [new File(["hello"], "a.txt", { type: "text/plain" })],
      { path: "raw/assets", headers: { "X-Request-Id": "upload-1" } },
    );

    const save = fetchMock.mock.calls[1][1]!;
    expect(fetchMock.mock.calls[1][0]).toBe(
      "/api/v2/projects/p1/files/content?path=wiki%2Fa.md",
    );
    expect(new Headers(save.headers).get("If-Match")).toBe("old");
    expect(JSON.parse(String(save.body))).toEqual({
      content: "next",
      revision: "old",
    });

    const remove = fetchMock.mock.calls[2][1]!;
    expect(fetchMock.mock.calls[2][0]).toBe(
      "/api/v2/projects/p1/files?path=wiki%2Fa.md",
    );
    expect(new Headers(remove.headers).get("If-Match")).toBe("new");
    expect(JSON.parse(String(remove.body))).toEqual({ revision: "new" });

    const upload = fetchMock.mock.calls[3][1]!;
    expect(fetchMock.mock.calls[3][0]).toBe(
      "/api/v2/projects/p1/uploads?path=raw%2Fassets",
    );
    expect(upload.body).toBeInstanceOf(FormData);
    expect((upload.body as FormData).getAll("files")).toHaveLength(1);
    expect(new Headers(upload.headers).get("Content-Type")).toBeNull();
    expect(new Headers(upload.headers).get("X-Request-Id")).toBe("upload-1");
    expect(client.files.assetUrl("p1", "raw/assets/a file.txt", true)).toBe(
      "/api/v2/projects/p1/assets?path=raw%2Fassets%2Fa+file.txt&download=true",
    );
  });

  it("normalizes server tree fields recursively", async () => {
    const client = createApiClient({
      fetch: vi.fn<typeof fetch>().mockResolvedValue(
        jsonResponse({
          tree: [
            {
              name: "wiki",
              path: "wiki",
              isDir: true,
              children: [
                {
                  name: "page.md",
                  path: "wiki/page.md",
                  kind: "file",
                  is_dir: false,
                },
              ],
            },
          ],
        }),
      ),
    });

    await expect(client.files.tree("p1")).resolves.toEqual([
      {
        name: "wiki",
        path: "wiki",
        kind: "directory",
        isDir: true,
        children: [
          { name: "page.md", path: "wiki/page.md", kind: "file", isDir: false },
        ],
      },
    ]);
  });

  it("matches project management, file creation and move, index, retry, and web chat setting contracts", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({ csrfToken: "csrf-1" }))
      .mockResolvedValueOnce(jsonResponse({ id: "p1", name: "Renamed" }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(
        jsonResponse({ path: "wiki/new.md", content: "", revision: "r1" }, 201),
      )
      .mockResolvedValueOnce(jsonResponse({ path: "wiki/renamed.md" }))
      .mockResolvedValueOnce(
        jsonResponse(
          { items: [{ path: "raw/sources/a.txt", revision: "r2", size: 5 }] },
          201,
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          {
            id: "j1",
            projectId: "p1",
            type: "index-rebuild",
            status: "queued",
          },
          202,
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          {
            id: "j2",
            projectId: "p1",
            type: "index-rebuild",
            status: "queued",
          },
          202,
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          webChat: {
            endpoint: "https://api.example/v1",
            model: "model",
            apiKey: { configured: true },
          },
        }),
      );
    const client = createApiClient({ fetch: fetchMock, baseUrl: "/api/v2" });

    await client.projects.update("p1", { name: "Renamed" });
    await client.projects.remove("p1");
    await client.files.createText("p1", "wiki/new.md");
    await client.files.move("p1", {
      sourcePath: "wiki/new.md",
      targetPath: "wiki/renamed.md",
    });
    await client.files.upload("p1", [
      new File(["hello"], "a.txt", { type: "text/plain" }),
    ]);
    await client.index.rebuild("p1");
    await client.jobs.retry("j1");
    await client.settings.updateWebChat({
      endpoint: "https://api.example/v1",
      model: "model",
      apiKey: "secret",
    });

    expect(fetchMock.mock.calls[1][0]).toBe("/api/v2/projects/p1");
    expect(fetchMock.mock.calls[1][1]).toMatchObject({ method: "PATCH" });
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({
      name: "Renamed",
    });
    expect(fetchMock.mock.calls[2][0]).toBe("/api/v2/projects/p1");
    expect(fetchMock.mock.calls[2][1]).toMatchObject({ method: "DELETE" });
    expect(fetchMock.mock.calls[3][0]).toBe(
      "/api/v2/projects/p1/files/content?path=wiki%2Fnew.md",
    );
    expect(JSON.parse(String(fetchMock.mock.calls[3][1]?.body))).toEqual({
      content: "",
      revision: "*",
    });
    expect(
      new Headers(fetchMock.mock.calls[3][1]?.headers).get("If-Match"),
    ).toBe("*");
    expect(fetchMock.mock.calls[4][0]).toBe("/api/v2/projects/p1/files/move");
    expect(JSON.parse(String(fetchMock.mock.calls[4][1]?.body))).toEqual({
      sourcePath: "wiki/new.md",
      targetPath: "wiki/renamed.md",
    });
    expect(fetchMock.mock.calls[5][0]).toBe(
      "/api/v2/projects/p1/uploads?path=raw%2Fsources",
    );
    expect(fetchMock.mock.calls[6][0]).toBe(
      "/api/v2/projects/p1/index/rebuild",
    );
    expect(fetchMock.mock.calls[7][0]).toBe("/api/v2/jobs/j1/retry");
    expect(fetchMock.mock.calls[8][0]).toBe("/api/v2/settings");
    expect(JSON.parse(String(fetchMock.mock.calls[8][1]?.body))).toEqual({
      webChat: {
        endpoint: "https://api.example/v1",
        model: "model",
        apiKey: "secret",
      },
    });
  });

  it("matches the register, review, chat, jobs, settings, and SSE contracts", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          new TextEncoder().encode(
            'event: delta\ndata: {"delta":"answer"}\n\nevent: done\ndata: "[DONE]"\n\n',
          ),
        );
        controller.close();
      },
    });
    const jobStream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          new TextEncoder().encode(
            'event: job\ndata: {"id":"j1","status":"queued"}\n\n',
          ),
        );
        controller.close();
      },
    });
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({ csrfToken: "csrf-1" }))
      .mockResolvedValueOnce(
        jsonResponse({ id: "p1", name: "Wiki", createdAt: 1 }),
      )
      .mockResolvedValueOnce(
        jsonResponse({ items: [{ id: "r1", title: "Review" }] }),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          id: "r1",
          title: "Review",
          status: "resolved",
          action: "accept",
          resolved: true,
        }),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          items: [{ id: "s1", title: "Chat", createdAt: 1, updatedAt: 1 }],
        }),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          id: "s2",
          title: "New chat",
          createdAt: 2,
          updatedAt: 2,
        }),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          id: "s2",
          messages: [{ id: "m1", role: "user", content: "Hi", createdAt: 2 }],
        }),
      )
      .mockResolvedValueOnce(
        new Response(stream, {
          headers: { "Content-Type": "text/event-stream" },
        }),
      )
      .mockResolvedValueOnce(
        new Response(jobStream, {
          headers: { "Content-Type": "text/event-stream" },
        }),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          items: [
            {
              id: "j1",
              projectId: "p1",
              type: "index",
              status: "queued",
              progress: {},
            },
          ],
        }),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          id: "j1",
          projectId: "p1",
          type: "index",
          status: "cancelled",
          progress: {},
        }),
      )
      .mockResolvedValueOnce(jsonResponse({ enabled: true }))
      .mockResolvedValueOnce(jsonResponse({ enabled: false }));
    const client = createApiClient({ fetch: fetchMock, baseUrl: "/api/v2" });
    const events: string[] = [];
    const jobEvents: string[] = [];

    await client.projects.register({ relativePath: "existing/wiki" });
    await client.reviews.list("p1");
    await client.reviews.resolve("p1", "r1", "accept");
    await client.chat.listSessions("p1");
    await client.chat.createSession("p1", { title: "New chat" });
    await client.chat.session("p1", "s2");
    await client.chat.streamTurn(
      "p1",
      "s2",
      { message: "Hi" },
      { onEvent: (event) => events.push(`${event.event}:${event.data}`) },
      { headers: { "X-Trace": "turn-1" } },
    );
    await client.jobs.streamProjectEvents("p1", {
      onEvent: (event) => jobEvents.push(`${event.event}:${event.data}`),
    });
    await client.jobs.list("p1");
    await client.jobs.cancel("j1");
    await client.settings.get();
    await client.settings.update({ enabled: false });

    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({
      relativePath: "existing/wiki",
    });
    expect(JSON.parse(String(fetchMock.mock.calls[3][1]?.body))).toEqual({
      status: "resolved",
      action: "accept",
    });
    expect(fetchMock.mock.calls[7][0]).toBe(
      "/api/v2/projects/p1/chat/sessions/s2/turns",
    );
    expect(new Headers(fetchMock.mock.calls[7][1]?.headers).get("Accept")).toBe(
      "text/event-stream",
    );
    expect(
      new Headers(fetchMock.mock.calls[7][1]?.headers).get("X-CSRF-Token"),
    ).toBe("csrf-1");
    expect(
      new Headers(fetchMock.mock.calls[7][1]?.headers).get("X-Trace"),
    ).toBe("turn-1");
    expect(JSON.parse(String(fetchMock.mock.calls[7][1]?.body))).toEqual({
      message: "Hi",
    });
    expect(fetchMock.mock.calls[8][0]).toBe("/api/v2/events?projectId=p1");
    expect(new Headers(fetchMock.mock.calls[8][1]?.headers).get("Accept")).toBe(
      "text/event-stream",
    );
    expect(fetchMock.mock.calls[9][0]).toBe("/api/v2/jobs?projectId=p1");
    expect(fetchMock.mock.calls[11][0]).toBe("/api/v2/settings");
    expect(JSON.parse(String(fetchMock.mock.calls[12][1]?.body))).toEqual({
      enabled: false,
    });
    expect(events).toEqual(['delta:{"delta":"answer"}', 'done:"[DONE]"']);
    expect(jobEvents).toEqual(['job:{"id":"j1","status":"queued"}']);
  });

  it("parses split SSE records and preserves event metadata", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          new TextEncoder().encode("id: 7\nevent: delta\ndata: hel"),
        );
        controller.enqueue(new TextEncoder().encode("lo\n\ndata: done\n\n"));
        controller.close();
      },
    });
    const events: Array<{ event: string; data: string; id?: string }> = [];

    await consumeSse(
      new Response(stream, {
        headers: { "Content-Type": "text/event-stream" },
      }),
      {
        onEvent: (event) => events.push(event),
      },
    );

    expect(events).toEqual([
      { id: "7", event: "delta", data: "hello", retry: undefined },
      { id: undefined, event: "message", data: "done", retry: undefined },
    ]);
  });
});
