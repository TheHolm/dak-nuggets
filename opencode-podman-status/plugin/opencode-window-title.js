/**
 * opencode-podman-status window-title plugin.
 *
 * Puts the container's hostname into the terminal/tmux window title, so
 * several opencode instances running in separate containers can be told apart
 * from the window list. opencode itself sets the title to `OpenCode` on the
 * home screen and `OC | <session title>` inside a session, with no way to
 * configure it (no config key; only OPENCODE_DISABLE_TERMINAL_TITLE and a
 * command-palette toggle). This plugin instead re-applies its own title:
 *
 *   home / no session   OpenCode (<hostname>)
 *   session             OpenCode (<hostname>) | <session title>
 *
 * `<hostname>` is HOSTNAME from the environment when set, else the system
 * hostname. In a rootless podman container with no `--hostname`, that is the
 * short container ID; pass `--hostname <name>` (or `-e HOSTNAME=<name>`) to
 * get a friendlier one.
 *
 * The session title is truncated the way opencode truncates its own
 * (`> 40` characters becomes the first 37 plus an ellipsis). If the
 * command-palette "terminal title" toggle is turned off, the title is left
 * cleared, exactly as core would.
 *
 * opencode's core keeps writing its own title whenever the route or a session
 * title changes, and offers no hook to stop it. Worse, it also writes once,
 * unconditionally, the first time its own effect runs after mount - which can
 * happen *after* this plugin's first write and clobber it even though nothing
 * this plugin cares about ever changed again. So this plugin does not try to
 * detect changes: it polls `route.current` and `state.session` once a second
 * and unconditionally re-applies its title every tick, which guarantees any
 * clobbering by core is corrected within one poll interval. It reaches the
 * renderer through the TUI plugin API (`@opencode-ai/plugin/tui`), which is
 * only available in the TUI - under `opencode serve`, `web` or the desktop
 * app this plugin does nothing.
 *
 * Enable it by listing this file's path in the `plugin` array of a `tui.json`
 * that opencode reads - see the program's README.markdown. It is deliberately
 * a separate file from the status plugin (`opencode-podman-status.js`), which
 * is a server plugin listed in `opencode.json`; the two can be enabled
 * independently.
 *
 * The default export is the TUI plugin module shape: `{ id, tui }`. Internals
 * are reachable for tests as `default.tui.internals`.
 *
 * @module opencode-window-title
 */

import { hostname } from "node:os"

/** Plugin id. */
const ID = "opencode-window-title"

/** How often the route/session state is re-read, in milliseconds. */
const POLL_MS = 1000

/** opencode core truncates a session title longer than this many characters. */
const TITLE_MAX = 40

/** How many characters survive core's truncation. */
const TITLE_KEEP = 37

/** Ellipsis appended by core's truncation. */
const ELLIPSIS = "\u2026"

/** KV key holding the command-palette "terminal title" toggle. */
const ENABLED_KV = "terminal_title_enabled"

/**
 * Resolves the hostname shown in the title.
 *
 * The environment's HOSTNAME is preferred because it can be set at container
 * launch (`-e HOSTNAME=...`) without changing the kernel hostname; the system
 * hostname is the fallback when it is unset or blank.
 *
 * @param {string|undefined} envHostname value of process.env.HOSTNAME
 * @param {string} sysHostname value of os.hostname()
 * @returns {string} the hostname, possibly empty, never padded
 */
function resolveHost(envHostname, sysHostname) {
  const env = typeof envHostname === "string" ? envHostname.trim() : ""
  if (env) return env
  return typeof sysHostname === "string" ? sysHostname.trim() : ""
}

/**
 * Truncates a session title the way opencode core does, so a long title does
 * not make the window title unusable.
 *
 * @param {string} title the raw session title
 * @returns {string} the title, trimmed, truncated to TITLE_MAX with an ellipsis
 */
function truncateTitle(title) {
  const t = typeof title === "string" ? title.trim() : ""
  if (t.length > TITLE_MAX) return t.slice(0, TITLE_KEEP) + ELLIPSIS
  return t
}

/**
 * Builds the window title for a hostname and an optional session title.
 *
 * The host prefix is always present; the session title is appended only when
 * there is one, so the home screen reads `OpenCode (<host>)` and a session
 * reads `OpenCode (<host>) | <title>`.
 *
 * @param {string} host resolved hostname, possibly empty
 * @param {string|undefined} sessionTitle raw session title, if any
 * @returns {string} the title to hand to the renderer
 */
function composeTitle(host, sessionTitle) {
  const base = host ? `OpenCode (${host})` : "OpenCode"
  const title = truncateTitle(sessionTitle)
  return title ? `${base} | ${title}` : base
}

/**
 * Reads the current state and decides what the title should be.
 *
 * @param {object} api the TuiPluginApi handed to the plugin
 * @param {string} host the resolved hostname
 * @returns {{enabled: boolean, title: string}} decision for this tick
 */
function evaluate(api, host) {
  const route = api?.route?.current
  const sessionID = route?.name === "session" ? route.params?.sessionID : undefined
  const rawTitle = sessionID ? api?.state?.session?.get(sessionID)?.title : undefined
  const enabled = api?.kv?.get?.(ENABLED_KV, true) ?? true
  const title = enabled ? composeTitle(host, rawTitle) : ""
  return { enabled, title }
}

/**
 * Starts applying the title, immediately and then on a timer.
 *
 * Every tick unconditionally re-applies the computed title, rather than only
 * on change: core writes its own title once, unconditionally, the first time
 * its own effect runs after mount, which can happen after this plugin's first
 * write and land later than it. Diffing against a "what did we last decide"
 * key cannot detect that clobber, since none of *this plugin's* inputs
 * changed - so any such write is only ever corrected by trying again
 * regardless of whether anything looked different. Timer and clock are
 * injectable so the tests can drive ticks by hand.
 *
 * @param {object} api the TuiPluginApi handed to the plugin
 * @param {object} [options] test hooks
 * @param {string} [options.host] hostname to use; resolved from the
 *   environment when omitted
 * @param {number} [options.pollMs] poll interval in milliseconds
 * @param {Function} [options.setIntervalFn] setInterval replacement
 * @param {Function} [options.clearIntervalFn] clearInterval replacement
 * @returns {{tick: Function, dispose: Function}} handles for tests
 */
function start(api, options = {}) {
  const host = options.host ?? resolveHost(process.env.HOSTNAME, hostname())
  const pollMs = options.pollMs ?? POLL_MS
  const setIntervalFn = options.setIntervalFn ?? setInterval
  const clearIntervalFn = options.clearIntervalFn ?? clearInterval

  /** Re-reads the state and (re-)writes the title, unconditionally. */
  const tick = () => {
    let decision
    try {
      decision = evaluate(api, host)
    } catch {
      return
    }
    try {
      api.renderer.setTerminalTitle(decision.title)
    } catch {
      // A title is cosmetic; never let it break the TUI loop.
    }
  }

  tick()
  const timer = setIntervalFn(tick, pollMs)
  /** Stops the poll timer. */
  const dispose = () => clearIntervalFn(timer)
  api?.lifecycle?.onDispose?.(dispose)

  return { tick, dispose }
}

/**
 * TUI plugin entry point. Registers the title poller and its cleanup.
 *
 * @param {object} api the TuiPluginApi handed to the plugin
 * @returns {Promise<void>} resolves once the poller is running
 */
const tui = async (api) => {
  start(api)
}

tui.internals = { ID, POLL_MS, resolveHost, truncateTitle, composeTitle, evaluate, start }

export default { id: ID, tui }
