/**
 * Tests for the window-title plugin. Run with `bun test` from this directory
 * (needs Bun: https://bun.sh). The API shape and title behaviour below are the
 * ones measured on a real opencode 1.18.33 TUI - see the program's NOTES.md.
 */

import { describe, expect, test } from "bun:test"
import module from "./opencode-window-title.js"

const { ID, POLL_MS, resolveHost, truncateTitle, composeTitle, evaluate, start } = module.tui.internals

/**
 * Minimal stand-in for the TuiPluginApi surface the plugin touches.
 *
 * @param {object} [state] initial route/enabled/session state
 * @returns {object} fake api, with `renderer.writes` and `disposed` recording calls
 */
function fakeApi(state = {}) {
  const { enabled = true, route = { name: "home" }, sessions = {} } = state
  const api = {
    kv: { get: (key, fallback) => (key === "terminal_title_enabled" ? enabled : fallback) },
    route: { current: route },
    state: { session: { get: (id) => sessions[id] } },
    renderer: { writes: [], setTerminalTitle(t) { this.writes.push(t) } },
    disposed: false,
  }
  api.lifecycle = { onDispose: (fn) => { api.dispose = fn } }
  return api
}

/**
 * Manual setInterval replacement so tests can fire ticks themselves.
 *
 * @returns {object} injectable timer functions plus a `fire` and `cleared`
 */
function fakeScheduler() {
  let scheduled = null
  const cleared = []
  return {
    setIntervalFn: (fn, ms) => {
      scheduled = { fn, ms }
      return 7
    },
    clearIntervalFn: (id) => cleared.push(id),
    fire: () => scheduled.fn(),
    scheduled: () => scheduled,
    cleared,
  }
}

describe("module shape", () => {
  /** The TUI plugin loader reads `default.tui`; `id` names the plugin. */
  test("default export is a TuiPluginModule", () => {
    expect(module.id).toBe(ID)
    expect(typeof module.tui).toBe("function")
    expect(typeof module.tui.internals).toBe("object")
  })
})

describe("resolveHost", () => {
  /** The launch-time HOSTNAME wins so a container can name itself. */
  test("prefers the environment hostname", () => {
    expect(resolveHost("web", "container-1")).toBe("web")
  })

  /** Without HOSTNAME, the kernel hostname is used. */
  test("falls back to the system hostname", () => {
    expect(resolveHost(undefined, "container-1")).toBe("container-1")
    expect(resolveHost("", "container-1")).toBe("container-1")
    expect(resolveHost("   ", "  web  ")).toBe("web")
  })

  /** Neither set yields an empty host, and the title drops the parentheses. */
  test("empty when neither is set", () => {
    expect(resolveHost(undefined, "")).toBe("")
    expect(resolveHost("", undefined)).toBe("")
  })
})

describe("truncateTitle", () => {
  /** A title at the limit is left alone. */
  test("keeps titles up to 40 characters", () => {
    expect(truncateTitle("short")).toBe("short")
    expect(truncateTitle("a".repeat(40))).toBe("a".repeat(40))
  })

  /** Over the limit matches core: first 37 characters plus an ellipsis. */
  test("truncates longer titles to 37 plus an ellipsis", () => {
    expect(truncateTitle("a".repeat(41))).toBe("a".repeat(37) + "\u2026")
    expect(truncateTitle("x".repeat(100))).toHaveLength(38)
  })

  /** Missing or blank titles become empty, not the string "undefined". */
  test("handles absent and blank titles", () => {
    expect(truncateTitle(undefined)).toBe("")
    expect(truncateTitle("  ")).toBe("")
  })
})

describe("composeTitle", () => {
  /** Home screen: host only. */
  test("host only without a session title", () => {
    expect(composeTitle("web", undefined)).toBe("OpenCode (web)")
    expect(composeTitle("web", "")).toBe("OpenCode (web)")
  })

  /** Session: host then the session title. */
  test("host and session title", () => {
    expect(composeTitle("web", "Fix the parser")).toBe("OpenCode (web) | Fix the parser")
  })

  /** No hostname at all: the prefix stays, without empty parentheses. */
  test("drops the parentheses when there is no host", () => {
    expect(composeTitle("", undefined)).toBe("OpenCode")
    expect(composeTitle("", "Fix the parser")).toBe("OpenCode | Fix the parser")
  })

  /** The session title is truncated like core's. */
  test("truncates the session title", () => {
    expect(composeTitle("web", "a".repeat(50))).toBe("OpenCode (web) | " + "a".repeat(37) + "\u2026")
  })
})

describe("evaluate", () => {
  /** Home route has no session, so no suffix. */
  test("home route", () => {
    const api = fakeApi()
    const { enabled, title } = evaluate(api, "web")
    expect(enabled).toBe(true)
    expect(title).toBe("OpenCode (web)")
  })

  /** Session route reads the title from state. */
  test("session route with a title", () => {
    const api = fakeApi({
      route: { name: "session", params: { sessionID: "ses_1" } },
      sessions: { ses_1: { title: "Fix the parser" } },
    })
    expect(evaluate(api, "web").title).toBe("OpenCode (web) | Fix the parser")
  })

  /** A session without a readable title or a missing session falls back to host only. */
  test("session route without a title", () => {
    const api = fakeApi({ route: { name: "session", params: { sessionID: "ses_1" } } })
    expect(evaluate(api, "web").title).toBe("OpenCode (web)")
  })

  /** The command-palette toggle is honoured: off means clear, as core would. */
  test("disabled terminal title clears", () => {
    const api = fakeApi({ enabled: false })
    const decision = evaluate(api, "web")
    expect(decision.enabled).toBe(false)
    expect(decision.title).toBe("")
  })
})

describe("start", () => {
  /** A write happens immediately, before the first timer tick. */
  test("writes on start", () => {
    const api = fakeApi()
    start(api, { host: "web", ...fakeScheduler() })
    expect(api.renderer.writes).toEqual(["OpenCode (web)"])
  })

  /**
   * The plugin does not diff against its own previous decision: it writes
   * every tick unconditionally. core writes its own title once, on the first
   * run of its own effect after mount, which can land after this plugin's
   * first write; diffing against "did our inputs change" could never detect
   * that clobber since none of them did. Always writing is what actually
   * fixes it - proven below by simulating exactly that clobber.
   */
  test("corrects a clobber from something else even with no input change", () => {
    const api = fakeApi()
    const scheduler = fakeScheduler()
    start(api, { host: "web", ...scheduler })
    api.renderer.setTerminalTitle("OpenCode") // core's own late first write
    scheduler.fire()
    expect(api.renderer.writes).toEqual(["OpenCode (web)", "OpenCode", "OpenCode (web)"])
  })

  /** A steady state keeps reasserting the same title on every tick. */
  test("keeps writing the same title on every tick", () => {
    const api = fakeApi()
    const scheduler = fakeScheduler()
    start(api, { host: "web", ...scheduler })
    scheduler.fire()
    scheduler.fire()
    expect(api.renderer.writes).toEqual(["OpenCode (web)", "OpenCode (web)", "OpenCode (web)"])
  })

  /** A route change is picked up on the next tick. */
  test("reapplies after a route change", () => {
    const api = fakeApi()
    const scheduler = fakeScheduler()
    start(api, { host: "web", ...scheduler })
    api.route.current = { name: "session", params: { sessionID: "ses_1" } }
    api.state.session.get = (id) => (id === "ses_1" ? { title: "Fix the parser" } : undefined)
    scheduler.fire()
    expect(api.renderer.writes).toEqual(["OpenCode (web)", "OpenCode (web) | Fix the parser"])
  })

  /** Turning the built-in title off clears it, and turning it back on restores. */
  test("honours the enabled toggle", () => {
    const api = fakeApi()
    const scheduler = fakeScheduler()
    start(api, { host: "web", ...scheduler })
    api.kv.get = (key, fallback) => (key === "terminal_title_enabled" ? false : fallback)
    scheduler.fire()
    expect(api.renderer.writes.at(-1)).toBe("")
  })

  /** The timer is registered and cleared on dispose. */
  test("polls on an interval and cleans up on dispose", () => {
    const api = fakeApi()
    const scheduler = fakeScheduler()
    const handle = start(api, { host: "web", ...scheduler })
    expect(scheduler.scheduled()).toEqual({ fn: handle.tick, ms: POLL_MS })
    api.dispose()
    expect(scheduler.cleared).toContain(7)
  })

  /** A renderer that throws must not break the poll loop. */
  test("swallows renderer errors", () => {
    const api = fakeApi()
    api.renderer.setTerminalTitle = () => {
      throw new Error("no tty")
    }
    expect(() => start(api, { host: "web", ...fakeScheduler() })).not.toThrow()
  })
})
