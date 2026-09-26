/**
 * Tests for the status plugin. Run with `bun test` from this directory (needs
 * Bun: https://bun.sh). The event sequences below are the ones captured from a
 * real opencode 1.18.32 - see the program's NOTES.md.
 */

import { describe, expect, test } from "bun:test"
import * as pluginModule from "./opencode-podman-status.js"

const { OpencodePodmanStatus } = pluginModule
const {
  MARKER,
  DEFAULT_PORT,
  MAX_SESSIONS,
  createTracker,
  createHandler,
  parsePort,
  requiredCredentials,
  authorized,
  constantTimeEqual,
} = OpencodePodmanStatus.internals

const S = "ses_f24b41bfbffeez685tI0ftEUMU"

/** A controllable clock for the tracker. */
function clock(start = 1_000) {
  let t = start
  const now = () => t
  now.advance = (ms) => (t += ms)
  return now
}

/** Shorthand for a session.status event. */
const status = (sessionID, type) => ({ type: "session.status", properties: { sessionID, status: { type } } })
const idle = (sessionID) => ({ type: "session.idle", properties: { sessionID } })
const error = (sessionID, name = "APIError") => ({
  type: "session.error",
  properties: { sessionID, error: { name, data: { message: "secret detail" } } },
})

/** A GET request for the handler. */
const get = (path, headers = {}) => new Request(`http://127.0.0.1:4097${path}`, { headers })

describe("module shape", () => {
  /** opencode refuses plugin modules with any non-function export. */
  test("exports exactly one function", () => {
    const exports = Object.values(pluginModule)
    expect(exports.length).toBe(1)
    expect(typeof exports[0]).toBe("function")
  })
})

describe("tracker", () => {
  /** Repeated busy events (opencode sends many per turn) keep the start time. */
  test("busy since is stamped once per turn", () => {
    const now = clock()
    const t = createTracker(now)
    t.handle(status(S, "busy"))
    now.advance(500)
    t.handle(status(S, "busy"))
    now.advance(500)
    t.handle(status(S, "retry"))
    expect(t.snapshot().statuses[S]).toEqual({ type: "retry", since_ms: 1_000 })
  })

  /** The real sequence of a normal turn ends idle, stamped when it ended. */
  test("turn ends idle", () => {
    const now = clock()
    const t = createTracker(now)
    t.handle(status(S, "busy"))
    now.advance(2_000)
    t.handle(status(S, "idle"))
    t.handle(idle(S))
    expect(t.snapshot().statuses[S]).toEqual({ type: "idle", since_ms: 3_000 })
  })

  /** Measured order: error, then idle. The error must survive the idle. */
  test("error persists through the idle that follows it", () => {
    const now = clock()
    const t = createTracker(now)
    t.handle(status(S, "busy"))
    now.advance(100)
    t.handle(error(S))
    now.advance(10)
    t.handle(status(S, "idle"))
    t.handle(idle(S))
    t.handle(status(S, "idle"))
    expect(t.snapshot().statuses[S]).toEqual({ type: "error", since_ms: 1_100 })
  })

  /** The next turn clears the error. */
  test("error is cleared by the next busy", () => {
    const now = clock()
    const t = createTracker(now)
    t.handle(error(S))
    now.advance(100)
    t.handle(status(S, "busy"))
    expect(t.snapshot().statuses[S]).toEqual({ type: "busy", since_ms: 1_100 })
  })

  /** A user abort is an operator action, not an error needing attention. */
  test("user abort is not an error", () => {
    const t = createTracker(clock())
    t.handle(status(S, "busy"))
    t.handle(error(S, "MessageAbortedError"))
    t.handle(idle(S))
    expect(t.snapshot().statuses[S].type).toBe("idle")
  })

  /** Error details (which can quote prompts or paths) never appear in output. */
  test("error details are not exposed", () => {
    const t = createTracker(clock())
    t.handle(error(S))
    expect(JSON.stringify(t.snapshot())).not.toContain("secret detail")
    expect(JSON.stringify(t.snapshot())).not.toContain("APIError")
  })

  /** Pending permissions and questions appear with their ask time, and go on reply. */
  test("tracks pending requests until replied", () => {
    const now = clock()
    const t = createTracker(now)
    t.handle({ type: "permission.asked", properties: { id: "per_1", sessionID: S, metadata: { command: "rm -rf /" } } })
    now.advance(50)
    t.handle({ type: "question.asked", properties: { id: "que_1", sessionID: S, questions: [{ question: "Pick" }] } })
    const snap = t.snapshot()
    expect(snap.permissions).toEqual([{ id: "per_1", since_ms: 1_000 }])
    expect(snap.questions).toEqual([{ id: "que_1", since_ms: 1_050 }])
    expect(JSON.stringify(snap)).not.toContain("rm -rf")
    expect(JSON.stringify(snap)).not.toContain("Pick")

    t.handle({ type: "permission.replied", properties: { sessionID: S, requestID: "per_1", reply: "once" } })
    t.handle({ type: "question.replied", properties: { sessionID: S, requestID: "que_1" } })
    expect(t.snapshot().permissions).toEqual([])
    expect(t.snapshot().questions).toEqual([])
  })

  /** question.rejected also resolves a question. */
  test("rejected question is no longer pending", () => {
    const t = createTracker(clock())
    t.handle({ type: "question.asked", properties: { id: "que_1", sessionID: S } })
    t.handle({ type: "question.rejected", properties: { sessionID: S, requestID: "que_1" } })
    expect(t.snapshot().questions).toEqual([])
  })

  /** A re-announced request keeps its original ask time. */
  test("repeated ask keeps the first time", () => {
    const now = clock()
    const t = createTracker(now)
    t.handle({ type: "permission.asked", properties: { id: "per_1", sessionID: S } })
    now.advance(500)
    t.handle({ type: "permission.asked", properties: { id: "per_1", sessionID: S } })
    expect(t.snapshot().permissions).toEqual([{ id: "per_1", since_ms: 1_000 }])
  })

  /**
   * Measured: aborting with a permission open emits no reply, and opencode's
   * own list keeps it forever. Going idle drops the dead prompt.
   */
  test("idle drops that session's pending requests only", () => {
    const t = createTracker(clock())
    t.handle({ type: "permission.asked", properties: { id: "per_1", sessionID: S } })
    t.handle({ type: "permission.asked", properties: { id: "per_2", sessionID: "ses_other" } })
    t.handle(error(S, "MessageAbortedError"))
    t.handle(status(S, "idle"))
    expect(t.snapshot().permissions).toEqual([{ id: "per_2", since_ms: 1_000 }])
  })

  /** A deleted session disappears, with its requests. */
  test("deleted session is forgotten", () => {
    const t = createTracker(clock())
    t.handle(status(S, "busy"))
    t.handle({ type: "permission.asked", properties: { id: "per_1", sessionID: S } })
    t.handle({ type: "session.deleted", properties: { sessionID: S, info: { id: S } } })
    expect(t.snapshot()).toEqual({ statuses: {}, questions: [], permissions: [] })
  })

  /** Only the most recent idle session is kept - enough for "idle since". */
  test("keeps only the newest idle session", () => {
    const now = clock()
    const t = createTracker(now)
    t.handle(idle("ses_a"))
    now.advance(10)
    t.handle(idle("ses_b"))
    t.handle(status("ses_c", "busy"))
    expect(Object.keys(t.snapshot().statuses).sort()).toEqual(["ses_b", "ses_c"])
  })

  /** Growth is capped however many sessions an instance sees. */
  test("caps tracked sessions", () => {
    const t = createTracker(clock())
    for (let i = 0; i < MAX_SESSIONS + 50; i++) t.handle(status(`ses_${i}`, "busy"))
    expect(Object.keys(t.snapshot().statuses).length).toBe(MAX_SESSIONS)
    expect(t.snapshot().statuses[`ses_${MAX_SESSIONS + 49}`]).toBeDefined()
  })

  /** Malformed and unrelated events change nothing and throw nothing. */
  test("ignores malformed events", () => {
    const t = createTracker(clock())
    for (const e of [
      undefined,
      null,
      {},
      { type: "session.status" },
      { type: "session.status", properties: { sessionID: 5, status: { type: "busy" } } },
      { type: "session.status", properties: { sessionID: S, status: { type: "hibernating" } } },
      { type: "session.error", properties: { sessionID: S } },
      { type: "permission.asked", properties: { id: "", sessionID: S } },
      { type: "message.updated", properties: { sessionID: S } },
    ]) {
      t.handle(e)
    }
    expect(t.snapshot()).toEqual({ statuses: {}, questions: [], permissions: [] })
  })
})

describe("handler", () => {
  /** The four routes answer with opencode-compatible shapes. */
  test("serves the four routes", async () => {
    const t = createTracker(clock())
    t.handle(status(S, "busy"))
    const h = createHandler(t, null)
    const health = await h(get("/global/health")).json()
    expect(health).toMatchObject({ healthy: true, source: MARKER })
    expect(await h(get("/session/status")).json()).toEqual({ [S]: { type: "busy", since_ms: 1_000 } })
    expect(await h(get("/question")).json()).toEqual([])
    expect(await h(get("/permission")).json()).toEqual([])
    expect(await h(get("/session/status?directory=/x")).json()).toEqual({ [S]: { type: "busy", since_ms: 1_000 } })
  })

  /** Everything else is 404, including opencode routes that change things. */
  test("refuses every other path", async () => {
    const h = createHandler(createTracker(clock()), null)
    for (const path of ["/", "/session", "/session/x/message", "/permission/per_1/reply", "/pty", "/file/content", "/event", "/doc"]) {
      expect(h(get(path)).status).toBe(404)
    }
  })

  /** Only GET is served: nothing can be written through this plugin. */
  test("refuses non-GET methods", async () => {
    const h = createHandler(createTracker(clock()), null)
    for (const method of ["POST", "PUT", "PATCH", "DELETE"]) {
      const r = h(new Request("http://127.0.0.1:4097/permission", { method, body: "{}" }))
      expect(r.status).toBe(405)
      expect(r.headers.get("allow")).toBe("GET")
    }
  })

  /** With a password configured, requests need the matching Basic credentials. */
  test("requires credentials when opencode has a password", async () => {
    const creds = requiredCredentials({ OPENCODE_SERVER_PASSWORD: "open sesame" })
    const h = createHandler(createTracker(clock()), creds)
    const basic = (s) => ({ authorization: `Basic ${Buffer.from(s).toString("base64")}` })
    expect(h(get("/global/health")).status).toBe(401)
    expect(h(get("/global/health")).headers.get("www-authenticate")).toContain("Basic")
    expect(h(get("/global/health", basic("opencode:wrong"))).status).toBe(401)
    expect(h(get("/global/health", basic("other:open sesame"))).status).toBe(401)
    expect(h(get("/global/health", { authorization: "Bearer open sesame" })).status).toBe(401)
    expect(h(get("/global/health", basic("opencode:open sesame"))).status).toBe(200)
    // Auth comes before routing, so unknown paths do not leak which exist.
    expect(h(get("/nope")).status).toBe(401)
  })
})

describe("configuration", () => {
  /** The credential rule mirrors opencode's: only a non-empty password counts. */
  test("credentials follow opencode's environment", () => {
    expect(requiredCredentials({})).toBeNull()
    expect(requiredCredentials({ OPENCODE_SERVER_PASSWORD: "" })).toBeNull()
    expect(requiredCredentials({ OPENCODE_SERVER_PASSWORD: "pw" })).toEqual({ username: "opencode", password: "pw" })
    expect(requiredCredentials({ OPENCODE_SERVER_PASSWORD: "pw", OPENCODE_SERVER_USERNAME: "me" })).toEqual({
      username: "me",
      password: "pw",
    })
  })

  /** No credentials configured means no authentication at all. */
  test("no password means open", () => {
    expect(authorized(get("/"), null)).toBe(true)
  })

  /** Comparison is by value, whatever the lengths. */
  test("constant-time comparison is still a comparison", () => {
    expect(constantTimeEqual("a:b", "a:b")).toBe(true)
    expect(constantTimeEqual("a:b", "a:c")).toBe(false)
    expect(constantTimeEqual("a:b", "a:bb")).toBe(false)
    expect(constantTimeEqual("", "")).toBe(true)
  })

  /** Only a plain port number is accepted; the default is 4097. */
  test("parses the port setting strictly", () => {
    expect(parsePort(undefined)).toBe(DEFAULT_PORT)
    expect(parsePort("")).toBe(DEFAULT_PORT)
    expect(parsePort("4097")).toBe(4097)
    expect(parsePort("65535")).toBe(65535)
    for (const bad of ["0", "65536", "-1", "40 97", "0x1001", "4097.0", "port", "999999"]) {
      expect(parsePort(bad)).toBeNull()
    }
  })
})

describe("live listener", () => {
  /**
   * The real entry point binds 127.0.0.1 on OPENCODE_STATUS_PORT, feeds events
   * through, and a second invocation in the same process shares both instead
   * of failing on the port.
   */
  test("serves events end to end on loopback only", async () => {
    const saved = { ...process.env }
    const probe = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() })
    const port = probe.port
    probe.stop(true)
    process.env.OPENCODE_STATUS_PORT = String(port)
    delete process.env.OPENCODE_SERVER_PASSWORD
    try {
      const hooks = await OpencodePodmanStatus({})
      const again = await OpencodePodmanStatus({})
      await hooks.event({ event: status(S, "busy") })
      await again.event({ event: { type: "permission.asked", properties: { id: "per_9", sessionID: S } } })
      const base = `http://127.0.0.1:${port}`
      expect((await (await fetch(`${base}/global/health`)).json()).source).toBe(MARKER)
      expect((await (await fetch(`${base}/session/status`)).json())[S].type).toBe("busy")
      expect((await (await fetch(`${base}/permission`)).json()).map((p) => p.id)).toEqual(["per_9"])
      const server = globalThis[Symbol.for("opencode-podman-status.shared")].server
      expect(server.hostname).toBe("127.0.0.1")
      // A malformed event must not throw into opencode.
      await hooks.event({ event: { type: "session.status", properties: null } })
      server.stop(true)
    } finally {
      process.env = saved
    }
  })
})
