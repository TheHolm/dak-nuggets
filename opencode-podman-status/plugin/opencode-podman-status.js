/**
 * opencode-podman-status status plugin.
 *
 * Serves a **read-only** subset of opencode's own HTTP API - only the routes
 * opencode-podman-status needs, reduced to state and `since_ms` - so opencode
 * can be monitored without being started with `--port`. `--port` exposes the
 * full remote-control API (answer permission prompts, run shell commands, open
 * a terminal, ...) to anything that can reach it, including the agent's own
 * tools inside the same container. This plugin exposes nothing that can change
 * anything: GET only, four routes, no titles, messages, paths or content.
 *
 * Routes, mirroring opencode's shapes:
 *
 *   GET /global/health   {healthy, version, source: "opencode-podman-status-plugin"}
 *   GET /session/status  {<sessionID>: {type: busy|retry|idle|error, since_ms}}
 *   GET /question        [{id, since_ms}]
 *   GET /permission      [{id, since_ms}]
 *
 * Unlike opencode's own `/session/status`, `error` is reported (opencode
 * delivers errors only as events, so its API cannot) and one `idle` entry - the
 * most recent - is kept so "idle since" can be answered.
 *
 * Listens on 127.0.0.1 only, never any other address, on OPENCODE_STATUS_PORT
 * (default 4097). If opencode itself was started with OPENCODE_SERVER_PASSWORD,
 * the same HTTP Basic credentials are required here
 * (OPENCODE_SERVER_USERNAME, default "opencode").
 *
 * Enable it by listing this file's path in the `plugin` array of an
 * opencode.json that opencode reads - see the program's README.markdown.
 *
 * This module deliberately exports exactly one thing: opencode treats every
 * export of a plugin module as a plugin and refuses non-functions. Internals
 * are reachable for tests as `OpencodePodmanStatus.internals`.
 *
 * @module opencode-podman-status
 */

import { createHash, timingSafeEqual } from "node:crypto"

/** Marker in /global/health that tells the helper it is talking to this plugin. */
const MARKER = "opencode-podman-status-plugin"

/** Plugin version, reported in /global/health. Kept in step with Cargo.toml. */
const VERSION = "0.2.0"

/** Port used when OPENCODE_STATUS_PORT is not set. */
const DEFAULT_PORT = 4097

/** The only address ever listened on. Not configurable, deliberately. */
const HOSTNAME = "127.0.0.1"

/** Caps on tracked entries, so a long-lived instance cannot grow without bound. */
const MAX_SESSIONS = 256
const MAX_REQUESTS = 256

/** Session states that mean "working". */
const WORKING = new Set(["busy", "retry"])

/** Key under which state is shared between plugin invocations in one process. */
const SHARED = Symbol.for("opencode-podman-status.shared")

/**
 * True for a non-empty string, the only kind of ID accepted from an event.
 * @param {unknown} v
 * @returns {v is string}
 */
function isId(v) {
  return typeof v === "string" && v.length > 0 && v.length <= 128
}

/**
 * Tracks session states and pending requests from opencode's bus events.
 *
 * `since_ms` is stamped when an entry *changes* state: opencode repeats
 * `session.status busy` many times per turn (measured on 1.18.32), and a repeat
 * must not reset the clock. Moving between `busy` and `retry` is still working
 * and keeps the original start.
 *
 * Measured event behaviour this relies on (opencode 1.18.32):
 * - `session.error` arrives *before* the `idle` that ends the turn, so an error
 *   persists through that idle and is cleared only by the session next becoming
 *   busy;
 * - a user abort also emits `session.error` (`MessageAbortedError`); that is an
 *   operator action, not something needing attention, so it is ignored;
 * - aborting a turn with a permission prompt open emits no reply event, and
 *   opencode's own `GET /permission` keeps listing it; so a session going idle
 *   drops that session's pending requests (they can no longer be answered).
 *
 * @param {() => number} [now] clock, injectable for tests
 */
function createTracker(now = Date.now) {
  /** @type {Map<string, {type: string, since_ms: number}>} */
  const sessions = new Map()
  /** @type {Map<string, {sessionID: string, since_ms: number}>} */
  const questions = new Map()
  /** @type {Map<string, {sessionID: string, since_ms: number}>} */
  const permissions = new Map()

  /**
   * Sets a session's state, keeping `since_ms` when nothing really changed.
   * @param {string} id
   * @param {string} type
   */
  function setState(id, type) {
    const current = sessions.get(id)
    const unchanged =
      current && (current.type === type || (WORKING.has(current.type) && WORKING.has(type)))
    if (unchanged) {
      if (current.type !== type) sessions.set(id, { type, since_ms: current.since_ms })
      return
    }
    sessions.delete(id) // re-insert so Map order is "least recently changed first"
    sessions.set(id, { type, since_ms: now() })
    prune()
  }

  /**
   * Handles the end of a turn: pending prompts die with it, and an error that
   * ended the turn stays visible.
   * @param {string} id
   */
  function becameIdle(id) {
    dropRequestsOf(id)
    if (sessions.get(id)?.type === "error") return
    setState(id, "idle")
  }

  /**
   * Forgets pending requests belonging to a session.
   * @param {string} id
   */
  function dropRequestsOf(id) {
    for (const map of [questions, permissions]) {
      for (const [key, value] of map) if (value.sessionID === id) map.delete(key)
    }
  }

  /**
   * Keeps only the most recent idle session (enough for "idle since") and caps
   * everything else, dropping the least recently changed first.
   */
  function prune() {
    const idle = [...sessions].filter(([, s]) => s.type === "idle")
    for (const [id] of idle.slice(0, -1)) sessions.delete(id)
    while (sessions.size > MAX_SESSIONS) sessions.delete(sessions.keys().next().value)
    for (const map of [questions, permissions]) {
      while (map.size > MAX_REQUESTS) map.delete(map.keys().next().value)
    }
  }

  /**
   * Records a newly asked question or permission.
   * @param {Map<string, {sessionID: string, since_ms: number}>} map
   * @param {Record<string, unknown>} p event properties
   */
  function asked(map, p) {
    if (!isId(p.id) || !isId(p.sessionID) || map.has(p.id)) return
    map.set(p.id, { sessionID: p.sessionID, since_ms: now() })
    prune()
  }

  /**
   * Applies one bus event. Unknown or malformed events are ignored.
   * @param {{type?: string, properties?: Record<string, any>}} event
   */
  function handle(event) {
    const p = event?.properties ?? {}
    switch (event?.type) {
      case "session.status": {
        const type = p.status?.type
        if (!isId(p.sessionID) || typeof type !== "string") return
        if (type === "idle") becameIdle(p.sessionID)
        else if (WORKING.has(type)) setState(p.sessionID, type)
        return
      }
      case "session.idle":
        if (isId(p.sessionID)) becameIdle(p.sessionID)
        return
      case "session.error":
        if (!isId(p.sessionID) || !p.error) return
        if (p.error.name === "MessageAbortedError") return
        setState(p.sessionID, "error")
        return
      case "session.deleted": {
        const id = isId(p.sessionID) ? p.sessionID : p.info?.id
        if (!isId(id)) return
        sessions.delete(id)
        dropRequestsOf(id)
        return
      }
      case "question.asked":
        return asked(questions, p)
      case "question.replied":
      case "question.rejected":
        if (isId(p.requestID)) questions.delete(p.requestID)
        return
      case "permission.asked":
        return asked(permissions, p)
      case "permission.replied":
        if (isId(p.requestID)) permissions.delete(p.requestID)
        return
    }
  }

  /**
   * The current state in the shapes the routes return. Nothing beyond IDs,
   * state words and timestamps ever leaves.
   */
  function snapshot() {
    /** @param {Map<string, {since_ms: number}>} map */
    const list = (map) => [...map].map(([id, v]) => ({ id, since_ms: v.since_ms }))
    return {
      statuses: Object.fromEntries(
        [...sessions].map(([id, s]) => [id, { type: s.type, since_ms: s.since_ms }]),
      ),
      questions: list(questions),
      permissions: list(permissions),
    }
  }

  return { handle, snapshot }
}

/**
 * Parses OPENCODE_STATUS_PORT. Returns `null` for anything but a whole number
 * in 1..65535, so a typo cannot silently bind somewhere unexpected.
 * @param {string | undefined} value
 * @returns {number | null}
 */
function parsePort(value) {
  if (value === undefined || value === "") return DEFAULT_PORT
  if (!/^[0-9]{1,5}$/.test(value)) return null
  const port = Number(value)
  return port >= 1 && port <= 65535 ? port : null
}

/**
 * The credentials this server requires, mirroring opencode's own rule: a
 * password is required only when OPENCODE_SERVER_PASSWORD is set and non-empty.
 * @param {Record<string, string | undefined>} env
 * @returns {{username: string, password: string} | null}
 */
function requiredCredentials(env) {
  const password = env.OPENCODE_SERVER_PASSWORD
  if (!password) return null
  return { username: env.OPENCODE_SERVER_USERNAME || "opencode", password }
}

/**
 * Compares two strings in time independent of where they differ. Hashing first
 * makes the inputs equal-length, which timingSafeEqual requires.
 * @param {string} a
 * @param {string} b
 */
function constantTimeEqual(a, b) {
  const digest = (s) => createHash("sha256").update(s, "utf8").digest()
  return timingSafeEqual(digest(a), digest(b))
}

/**
 * True if the request carries the required Basic credentials (or none are
 * required).
 * @param {Request} request
 * @param {{username: string, password: string} | null} credentials
 */
function authorized(request, credentials) {
  if (!credentials) return true
  const header = request.headers.get("authorization") ?? ""
  const match = /^Basic ([A-Za-z0-9+/=]+)$/i.exec(header.trim())
  const presented = match ? Buffer.from(match[1], "base64").toString("utf8") : ""
  // Compare the whole "user:password" string, so neither part short-circuits.
  return constantTimeEqual(presented, `${credentials.username}:${credentials.password}`)
}

/**
 * A JSON response with headers that keep it out of caches.
 * @param {number} status
 * @param {unknown} body
 * @param {Record<string, string>} [headers]
 */
function json(status, body, headers = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", ...headers },
  })
}

/**
 * Builds the request handler: authentication, then a fixed GET-only route table.
 * @param {ReturnType<typeof createTracker>} tracker
 * @param {{username: string, password: string} | null} credentials
 * @returns {(request: Request) => Response}
 */
function createHandler(tracker, credentials) {
  return (request) => {
    if (!authorized(request, credentials)) {
      return json(401, { error: "unauthorized" }, { "www-authenticate": 'Basic realm="opencode-podman-status"' })
    }
    const path = new URL(request.url).pathname
    const routes = {
      "/global/health": () => ({ healthy: true, version: VERSION, source: MARKER }),
      "/session/status": () => tracker.snapshot().statuses,
      "/question": () => tracker.snapshot().questions,
      "/permission": () => tracker.snapshot().permissions,
    }
    if (!Object.hasOwn(routes, path)) return json(404, { error: "not found" })
    if (request.method !== "GET") return json(405, { error: "method not allowed" }, { allow: "GET" })
    return json(200, routes[path]())
  }
}

/**
 * The state shared by every invocation of the plugin in this process.
 *
 * opencode may initialise a plugin once per project instance within one
 * process; they must feed one tracker and share one listener rather than
 * fight over the port.
 */
function shared() {
  globalThis[SHARED] ??= { tracker: createTracker(), server: null, started: false }
  return globalThis[SHARED]
}

/**
 * Writes to opencode's log, never throwing: logging must not break opencode.
 * @param {any} client
 * @param {"info" | "warn" | "error"} level
 * @param {string} message
 */
async function log(client, level, message) {
  try {
    await client?.app?.log?.({ body: { service: "opencode-podman-status", level, message } })
  } catch {
    // Nothing sensible to do; the plugin must stay out of opencode's way.
  }
}

/**
 * Starts the listener once per process. Any failure is logged and swallowed:
 * a monitoring plugin must never stop opencode from starting.
 * @param {any} client
 * @param {Record<string, string | undefined>} env
 */
async function start(client, env) {
  const state = shared()
  if (state.started) return
  state.started = true
  const port = parsePort(env.OPENCODE_STATUS_PORT)
  if (port === null) {
    await log(client, "error", `invalid OPENCODE_STATUS_PORT ${JSON.stringify(env.OPENCODE_STATUS_PORT)}; status plugin not serving`)
    return
  }
  try {
    state.server = Bun.serve({
      hostname: HOSTNAME,
      port,
      fetch: createHandler(state.tracker, requiredCredentials(env)),
    })
    await log(client, "info", `status plugin serving on ${HOSTNAME}:${port}`)
  } catch (e) {
    await log(client, "error", `status plugin cannot listen on ${HOSTNAME}:${port}: ${e}`)
  }
}

/**
 * The plugin entry point opencode calls.
 * @param {{client?: any}} input
 */
export const OpencodePodmanStatus = async ({ client } = {}) => {
  await start(client, process.env)
  const { tracker } = shared()
  return {
    /** Feeds every bus event to the tracker; never throws into opencode. */
    event: async ({ event }) => {
      try {
        tracker.handle(event)
      } catch {
        // A malformed event must not disturb opencode.
      }
    },
  }
}

OpencodePodmanStatus.internals = {
  MARKER,
  VERSION,
  DEFAULT_PORT,
  HOSTNAME,
  MAX_SESSIONS,
  MAX_REQUESTS,
  createTracker,
  createHandler,
  parsePort,
  requiredCredentials,
  authorized,
  constantTimeEqual,
}
