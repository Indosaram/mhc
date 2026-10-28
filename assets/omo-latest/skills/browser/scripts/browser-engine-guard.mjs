// Engine policy for the omowright library when the host pins a browser engine through OMO_BROWSER_ENGINE.
//
// Unset or empty: the raw library is handed back untouched.
// Set: a new object is built in which only read-only helpers survive, the owned engine and every other acting
// export are replaced by refusing stubs, and connectBrowserSkill() returns a session wrapper that reports its
// state to the host, honours the user's Stop, and asks the user before irreversible actions.
//
// This prevents mistakes by a cooperating agent. It is not a security boundary.

export const BROWSER_ENGINE_ENV = "OMO_BROWSER_ENGINE"
export const BROWSER_CONFIRMATION_POLICY = "ask-before-irreversible"
export const BROWSER_STATE_EVENT = "omo.browser.state"
export const BROWSER_BRIDGE_TOOL = "omo_browser_bridge"
export const BROWSER_QUESTION_TOOLS = Object.freeze(["ask_user_question", "request_user_input"])

const ENGINES = Object.freeze(["connected", "builtin", "none"])

// ---------------------------------------------------------------------------------------------------------------
// Typed errors

class BrowserGuardError extends Error {
  constructor(message, code, options) {
    super(message, options)
    this.name = new.target.name
    this.code = code
  }
}

export class BrowserEngineRefusal extends BrowserGuardError {
  constructor(message, code, { engine, cause } = {}) {
    super(message, code, cause === undefined ? undefined : { cause })
    if (engine !== undefined) this.engine = engine
  }
}

export class BrowserNotConnectedError extends BrowserGuardError {
  constructor(
    message = "No browser is connected. Ask the user to open the browser that has the BrowserSkill extension enabled, then try again. Do not launch or use any other browser.",
    options,
  ) {
    super(message, "browser_not_connected", options)
  }
}

export class BrowserUserStoppedError extends BrowserGuardError {
  constructor(
    message = "The user stopped browser use. Report this to the user; no new browser session may be started this turn.",
    options,
  ) {
    super(message, "user_stopped", options)
  }
}

export class BrowserActionDeclinedError extends BrowserGuardError {
  constructor(message, code = "user_declined", options) {
    super(
      message ??
        (code === "confirmation_unavailable"
          ? "Nothing was done: this action needs the user's confirmation and nobody could be asked. Tell the user what you were about to do."
          : "Nothing was done: the user did not allow this action. Report it and do not retry it or look for another way to do it."),
      code,
      options,
    )
  }
}

// ---------------------------------------------------------------------------------------------------------------
// Irreversible-action labels

const ENGLISH_IRREVERSIBLE = [
  /\b(?:send|post|publish|tweet|reply)\b/i,
  /\b(?:pay|purchase|buy|subscribe|donate|checkout|submit|confirm|transfer|approve|merge)\b/i,
  /\be-?sign\b/i,
  /\bsign\s+(?:(?:the|this|your)\s+)?(?:document|contract|agreement|transaction|petition|form)s?\b/i,
  /\border\b(?!\s+(?:history|status|details|number|tracking)\b)/i,
  /\b(?:delete|remove|unsubscribe|deactivate)\b/i,
  /\bcancel\s+(?:(?:my|your)\s+)?(?:subscription|membership|plan|account)\b/i,
  /\bclose\s+(?:(?:my|your)\s+)?account\b/i,
]
// Korean: payment, purchase, order, subscribe, delete, remove, leave membership, send, dispatch, post, publish,
// submit, approve, remit, wire transfer, merge. Substring matches, since Korean attaches particles to words.
const KOREAN_IRREVERSIBLE = /결제|구매|주문|구독|삭제|제거|탈퇴|보내기|전송|발송|게시|발행|제출|승인|송금|이체|병합/

export function isIrreversibleLabel(label) {
  if (typeof label !== "string" || label.trim() === "") return false
  return ENGLISH_IRREVERSIBLE.some((pattern) => pattern.test(label)) || KOREAN_IRREVERSIBLE.test(label)
}

function firstIrreversible(labels) {
  return labels.find((label) => isIrreversibleLabel(label))
}

// ---------------------------------------------------------------------------------------------------------------
// Reading what an action is aimed at

const REF_PATTERN = /^@?e\d+$/

function classifyTarget(target) {
  if (target === undefined || target === null) return { kind: "none" }
  if (typeof target === "string") return REF_PATTERN.test(target) ? { kind: "ref", ref: target } : { kind: "selector", selector: target }
  if (typeof target === "object") {
    if (typeof target.ref === "string") return { kind: "ref", ref: target.ref }
    if (typeof target.selector === "string") return { kind: "selector", selector: target.selector }
  }
  return { kind: "point" }
}

const HTML_CAP = 4096

function decodeEntities(text) {
  return text
    .replace(/&nbsp;/g, " ")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;|&apos;/g, "'")
    .replace(/&amp;/g, "&")
}

function attributeOf(openTag, name) {
  const match = new RegExp(`\\s${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s>]+))`, "i").exec(openTag)
  if (!match) return undefined
  return decodeEntities(match[1] ?? match[2] ?? match[3] ?? "")
}

function hasAttribute(openTag, name) {
  return new RegExp(`\\s${name}(?:\\s*=|[\\s>/])`, "i").test(openTag)
}

// Labels and editability of an element, from its outer HTML.
export function describeElementHtml(html) {
  const source = String(html).slice(0, HTML_CAP)
  const openTag = /^\s*<[^>]*>/.exec(source)?.[0] ?? ""
  const tag = /^\s*<\s*([a-z0-9-]+)/i.exec(openTag)?.[1]?.toLowerCase()
  const labels = ["aria-label", "title", "value", "alt"].map((name) => attributeOf(openTag, name)).filter((v) => v)
  const text = decodeEntities(source.replace(/<[^>]*>/g, " ")).replace(/\s+/g, " ").trim()
  if (text) labels.push(text)
  const editableAttr = attributeOf(openTag, "contenteditable")
  const contentEditable = hasAttribute(openTag, "contenteditable") && (editableAttr ?? "").toLowerCase() !== "false"
  const editable = contentEditable || (attributeOf(openTag, "role") ?? "").toLowerCase() === "textbox" || tag === "textarea"
  return { tag, labels, editable }
}

async function readRef(raw, ref) {
  try {
    const result = await raw.getHtml({ ref, maxBytes: HTML_CAP })
    const html = typeof result === "string" ? result : result?.html ?? result?.value
    if (typeof html !== "string" || html.trim() === "") return undefined
    return describeElementHtml(html)
  } catch {
    return undefined
  }
}

// Runs in the page. Returns null for nothing focused / body / html.
function describerSource(selector) {
  return `(() => {
  const selector = ${JSON.stringify(selector ?? null)};
  const el = selector === null ? document.activeElement : document.querySelector(selector);
  if (!el || el === document.body || el === document.documentElement) return null;
  const tag = el.tagName.toLowerCase();
  const type = (el.getAttribute("type") || "").toLowerCase();
  const textTypes = ["", "text", "search", "email", "url", "tel", "password", "number"];
  const editable = el.isContentEditable || (el.getAttribute("role") || "").toLowerCase() === "textbox" ||
    tag === "textarea" || (tag === "input" && textTypes.includes(type));
  const buttonish = tag === "button" || (tag === "input" && ["submit", "button", "image"].includes(type));
  const text = (el.innerText || el.textContent || "").trim().slice(0, 200);
  const form = el.form || el.closest("form");
  let owner = null;
  if (form) {
    const submitLabels = [];
    for (const node of form.querySelectorAll("button, input[type=submit], input[type=image]")) {
      const t = (node.getAttribute("type") || (node.tagName === "BUTTON" ? "submit" : "")).toLowerCase();
      if (t !== "submit" && t !== "image") continue;
      const label = (node.tagName === "BUTTON" ? (node.innerText || node.textContent) : (node.value || node.alt)) || "";
      if (label.trim()) submitLabels.push(label.trim());
    }
    owner = { action: form.getAttribute("action") || "", method: (form.getAttribute("method") || "get").toLowerCase(), submitLabels };
  }
  return {
    tag, editable, text,
    ariaLabel: el.getAttribute("aria-label") || "",
    title: el.getAttribute("title") || "",
    value: buttonish ? String(el.value || "") : "",
    form: owner,
  };
})()`
}

const UNREADABLE = Symbol("unreadable")

async function describeInPage(raw, selector) {
  try {
    const result = await raw.evaluate(describerSource(selector), { awaitPromise: true })
    if (result && typeof result === "object" && "ok" in result) {
      if (result.ok !== true) return UNREADABLE
      return result.value ?? null
    }
    return result ?? null
  } catch {
    return UNREADABLE
  }
}

function ownLabels(described) {
  return [described.text, described.ariaLabel, described.title, described.value].filter((v) => typeof v === "string" && v)
}

const WRITE_PATH_SEGMENT = /(?:^|[-_.])(?:send|post|publish|pay|purchase|order|subscribe|delete|remove)(?:[-_.]|s?$)/i

function actionPathIsWrite(action) {
  let path = action
  try {
    path = new URL(action, "http://placeholder.invalid/").pathname
  } catch {}
  return path.split("/").some((segment) => segment && WRITE_PATH_SEGMENT.test(segment))
}

// ---------------------------------------------------------------------------------------------------------------
// Preflight checks. Each returns undefined to proceed, or a description of the action to confirm.

const quote = (label) => `"${label.length > 80 ? `${label.slice(0, 77)}...` : label}"`

async function clickPreflight(raw, [target]) {
  const t = classifyTarget(target)
  let labels
  if (t.kind === "ref") labels = (await readRef(raw, t.ref))?.labels
  else if (t.kind === "selector") {
    const described = await describeInPage(raw, t.selector)
    // A selector that resolves to nothing (or to body/html) has no label to match.
    if (described === null) labels = []
    else if (described !== UNREADABLE && typeof described === "object") labels = ownLabels(described)
  }
  if (labels === undefined) return "click a control whose label could not be read"
  const hit = firstIrreversible(labels)
  return hit === undefined ? undefined : `click ${quote(hit)}`
}

const SEND_MODIFIERS = new Set(["control", "ctrl", "meta", "cmd", "command"])

async function pressPreflight(raw, [key, options = {}]) {
  if (typeof key !== "string") return undefined
  const parts = key.split("+").map((part) => part.trim()).filter(Boolean)
  const last = parts.at(-1)?.toLowerCase()
  if (last !== "enter" && last !== "return") return undefined
  const modifiers = [...parts.slice(0, -1), ...(Array.isArray(options?.modifiers) ? options.modifiers : [])].map((m) =>
    String(m).toLowerCase(),
  )
  if (modifiers.some((m) => SEND_MODIFIERS.has(m))) return `press ${key} (a shortcut that sends in many apps)`
  if (modifiers.includes("shift")) return undefined

  const t = classifyTarget(options?.target)
  if (t.kind === "ref" || t.kind === "point") {
    const read = t.kind === "ref" ? await readRef(raw, t.ref) : undefined
    if (read === undefined) return "press Enter on a control whose label could not be read"
    if (read.editable) return "press Enter in a text box (this sends the message in many apps)"
    const hit = firstIrreversible(read.labels)
    return hit === undefined ? undefined : `press Enter on ${quote(hit)}`
  }

  const described = await describeInPage(raw, t.kind === "selector" ? t.selector : undefined)
  if (described === UNREADABLE || typeof described !== "object") return "press Enter on a control that could not be read"
  if (described === null) return undefined
  const form = described.form
  if (described.editable && (!form || form.method !== "get")) return "press Enter in a text box (this sends the message in many apps)"
  if (described.tag !== "input") {
    const hit = firstIrreversible(ownLabels(described))
    if (hit !== undefined) return `press Enter on ${quote(hit)}`
  }
  if (!form) return undefined
  const submit = firstIrreversible(Array.isArray(form.submitLabels) ? form.submitLabels : [])
  if (submit !== undefined) return `press Enter, which submits a form whose button says ${quote(submit)}`
  if (form.method === "post" && typeof form.action === "string" && actionPathIsWrite(form.action)) {
    return `press Enter, which posts a form to ${form.action}`
  }
  return undefined
}

const UI_DRIVING_SCRIPT = /\.click\s*\(|\.submit\s*\(|\.requestSubmit\s*\(|dispatchEvent|sendBeacon\s*\(/
const NETWORK_SCRIPT = /\bfetch\b|XMLHttpRequest/
const WRITE_METHOD_LITERAL = /(["'`])(?:POST|PUT|PATCH|DELETE)\1/i

export function scriptNeedsConfirmation(expression) {
  if (typeof expression !== "string") return false
  return UI_DRIVING_SCRIPT.test(expression) || (NETWORK_SCRIPT.test(expression) && WRITE_METHOD_LITERAL.test(expression))
}

async function evaluatePreflight(_raw, [expression]) {
  return scriptNeedsConfirmation(expression) ? "run a script that clicks, submits or sends data on the page" : undefined
}

// ---------------------------------------------------------------------------------------------------------------
// Host bridge and questions

function toolName(entry) {
  return typeof entry === "string" ? entry : entry?.name
}

function makeBridge(host, engine) {
  const call = async (args) => {
    if (!host || typeof host.callTool !== "function") return undefined
    try {
      const result = await host.callTool(BROWSER_BRIDGE_TOOL, args)
      if (!result || result.isError === true || result.error) return undefined
      return result.details ?? result
    } catch {
      return undefined
    }
  }
  return {
    async emit(status, sessionId, extras = {}) {
      const data = { engine, status }
      if (sessionId !== undefined) data.session = sessionId
      for (const key of ["action", "reason", "tab"]) if (extras[key] !== undefined) data[key] = extras[key]
      await call({ op: "state", data })
    },
    async isStopped() {
      const details = await call({ op: "status" })
      return details?.stopped === true
    },
    async markStopped() {
      await call({ op: "stopped" })
    },
  }
}

async function findQuestionTool(host) {
  if (!host || typeof host.listTools !== "function" || typeof host.callTool !== "function") return undefined
  let names
  try {
    names = new Set((await host.listTools()).map(toolName).filter((n) => typeof n === "string"))
  } catch {
    return undefined
  }
  return BROWSER_QUESTION_TOOLS.find((name) => names.has(name))
}

function hostOf(url) {
  try {
    return new URL(url).host || undefined
  } catch {
    return undefined
  }
}

const ALLOW = "Allow"
const DENY = "Don't allow"

function readAnswer(tool, result) {
  const details = result?.details ?? result
  if (details?.status !== "answered") return undefined
  const answers = details.answers
  const entries = Array.isArray(answers) ? answers : answers && typeof answers === "object" ? Object.values(answers) : []
  if (entries.length !== 1) return undefined
  const [entry] = entries
  if (tool === "ask_user_question") return entry
  return Array.isArray(entry?.answers) ? entry.answers[0] : undefined
}

// ---------------------------------------------------------------------------------------------------------------
// Guarded session

const NAVIGATING = ["navigate", "back", "forward", "reload", "waitForNavigation", "tabCreate", "tabSelect", "tabClose", "tabBorrow", "tabReturn"]
const PLAIN = ["fill", "select", "hover", "focus", "blur", "scrollTo", "wheel"]
const PREFLIGHT = { click: clickPreflight, press: pressPreflight, evaluate: evaluatePreflight }
const READ_ONLY_TOOLS = new Set(["observe", "snapshot", "get_html", "screenshot", "tab_list", "console", "network"])
const TOOL_METHOD_HINT = {
  click: "session.click()",
  fill: "session.fill()",
  type: "session.fill()",
  press: "session.press()",
  select: "session.select()",
  navigate: "session.navigate()",
  evaluate: "session.evaluate()",
  hover: "session.hover()",
}

async function currentTab(raw) {
  try {
    const listed = await raw.tabList({ scope: "agent" })
    const tabs = Array.isArray(listed) ? listed : listed?.tabs
    if (!Array.isArray(tabs) || tabs.length === 0) return undefined
    const tab = tabs.find((t) => t?.active === true) ?? tabs[0]
    const info = { id: tab.id, url: tab.url, title: tab.title }
    const favicon = tab.favIconUrl ?? tab.faviconUrl ?? tab.favicon ?? tab.favIcon ?? tab.iconUrl
    if (favicon) info.favicon = favicon
    return info
  } catch {
    return undefined
  }
}

function guardSession(raw, { bridge, host }) {
  const sessionId = () => raw?.sessionId
  let stopped = false

  const stopRaw = async () => {
    stopped = true
    try {
      await raw.stop()
    } catch {}
  }

  const confirm = async (action, description) => {
    const tool = await findQuestionTool(host)
    if (tool === undefined) throw new BrowserActionDeclinedError(undefined, "confirmation_unavailable")
    const tab = await currentTab(raw)
    await bridge.emit("awaiting_confirmation", sessionId(), { action, tab })
    const where = tab?.url ? hostOf(tab.url) : undefined
    const question = `Allow the agent to ${description}${where ? ` on ${where}` : ""}?`
    const options = [
      { label: ALLOW, description: "Let the agent do this once." },
      { label: DENY, description: "Nothing is done; the agent reports back." },
    ]
    const payload =
      tool === "ask_user_question"
        ? { waitForAnswer: true, questions: [{ header: "Browser", question, multiSelect: false, options }] }
        : { wait_for_answer: true, questions: [{ id: "confirm_browser_action", header: "Browser", question, options }] }
    let answer
    try {
      answer = readAnswer(tool, await host.callTool(tool, payload))
    } catch {
      answer = undefined
    }
    if (answer !== ALLOW) {
      await bridge.emit("failed", sessionId(), { action, reason: "user_declined" })
      throw new BrowserActionDeclinedError(undefined, "user_declined")
    }
  }

  const run = (action, { navigating = false, preflight } = {}) =>
    async function guardedAction(...args) {
      if (await bridge.isStopped()) {
        await stopRaw()
        throw new BrowserUserStoppedError()
      }
      if (preflight) {
        const description = await preflight(raw, args)
        if (description !== undefined) await confirm(action, description)
      }
      await bridge.emit("acting", sessionId(), { action })
      let result
      try {
        result = await raw[action](...args)
      } catch (error) {
        if (error?.code === "user_aborted") {
          await bridge.markStopped()
          await bridge.emit("user_stopped", sessionId(), { action })
          await stopRaw()
          throw new BrowserUserStoppedError(undefined, { cause: error })
        }
        await bridge.emit("failed", sessionId(), { action, reason: error?.code ?? error?.name })
        throw error
      }
      if (navigating) {
        await bridge.emit("navigated", sessionId(), { action, tab: await currentTab(raw) })
        await bridge.emit("idle", sessionId())
      }
      return result
    }

  const handlers = {}
  for (const name of NAVIGATING) handlers[name] = run(name, { navigating: true })
  for (const name of PLAIN) handlers[name] = run(name)
  for (const [name, preflight] of Object.entries(PREFLIGHT)) handlers[name] = run(name, { preflight })

  handlers.tool = async (name, params) => {
    if (!READ_ONLY_TOOLS.has(name)) {
      const hint = TOOL_METHOD_HINT[name] ?? "the matching session method"
      throw new BrowserEngineRefusal(
        `session.tool("${name}") is blocked while ${BROWSER_ENGINE_ENV} is set: raw tools are read-only here (${[...READ_ONLY_TOOLS].join(", ")}). Use ${hint} instead, which applies the confirmation policy.`,
        "browser_tool_blocked",
      )
    }
    return await raw.tool(name, params)
  }

  handlers.stop = async (...args) => {
    if (stopped) return null
    stopped = true
    try {
      return await raw.stop(...args)
    } finally {
      await bridge.emit("stopped", sessionId())
    }
  }

  return new Proxy(raw, {
    get(target, prop, receiver) {
      if (typeof prop === "string" && Object.hasOwn(handlers, prop)) return handlers[prop]
      const value = Reflect.get(target, prop, target)
      return typeof value === "function" ? value.bind(target) : value
    },
  })
}

// ---------------------------------------------------------------------------------------------------------------
// Guarded library

const PASS_THROUGH = new Set([
  "bskDoctor", "bskOnboard", "installBskCli", "readDaemonInfo", "resolveBskHome",
  "listBrowsers", "catalogBrowsers", "detectBrowsers", "identifyBrowser", "probeBrowserSignals",
  "findCloakBrowserPath", "resolveCloakProfile",
  "registerExternalExtension", "unregisterExternalExtension", "externalExtensionEntry",
  "buildSnapshotExpression", "buildCloakBrowserArgs", "compactSnapshot", "snapshotTokens",
  "describeLayers", "layersHeader", "decodeFrames", "encodeFrame", "reconcileFrames",
  "normalizeDialogPolicy", "resolveDialogAction", "sanitizeCookies", "toHar",
  "BskRpcError", "BridgeProtocolError", "BridgeUnavailableError", "BrowserCdpCommandError", "UnsupportedOperationError",
])
const OWNED_ENGINE = new Set(["connect", "connectPipe", "connectCloakProfile"])

function ownedEngineReason(engine) {
  switch (engine) {
    case "none":
      return "agent browser access is turned off for this project"
    case "builtin":
      return "this session uses the app's built-in browser, so use its in-app browser tools"
    case "connected":
      return "this session may only drive the user's own browser through connectBrowserSkill()"
    default:
      return `"${engine}" is not a supported engine value (expected one of ${ENGINES.join(", ")})`
  }
}

function blockedStub(name, engine) {
  const makeError = () =>
    OWNED_ENGINE.has(name)
      ? new BrowserEngineRefusal(
          `${name}() cannot launch a browser of your own in this session: ${ownedEngineReason(engine)}. Tell the user; do not look for another way to open a browser.`,
          "browser_engine_owned_blocked",
          { engine },
        )
      : new BrowserEngineRefusal(
          `${name} is unavailable while ${BROWSER_ENGINE_ENV} is set, because it could act on a browser outside the engine and confirmation policy of this session.`,
          "browser_engine_export_blocked",
          { engine },
        )
  const stub = function (..._args) {
    if (new.target) throw makeError()
    return Promise.reject(makeError())
  }
  Object.defineProperty(stub, "name", { value: name, configurable: true })
  return stub
}

function engineRefusal(engine) {
  if (engine === "none") {
    return new BrowserEngineRefusal(
      "Agent browser access is turned off for this project. Tell the user; do not try another browser.",
      "browser_engine_none",
      { engine },
    )
  }
  if (engine === "builtin") {
    return new BrowserEngineRefusal(
      "This session uses the app's built-in browser. Use the app's own in-app browser tools instead of this library.",
      "browser_engine_builtin",
      { engine },
    )
  }
  return new BrowserEngineRefusal(
    `${BROWSER_ENGINE_ENV}="${engine}" is not supported; valid values are ${ENGINES.join(", ")}. Tell the user.`,
    "browser_engine_unsupported",
    { engine },
  )
}

const NOT_CONNECTED_CODES = new Set(["no_browser_connected", "no_daemon", "unsupported"])

export function guardOmowright(raw, { env = process.env, host } = {}) {
  const engine = env[BROWSER_ENGINE_ENV]
  if (engine === undefined || engine === "") return raw

  const bridge = makeBridge(host, engine)
  const rawSessions = new WeakMap()
  const guarded = {}

  for (const [name, value] of Object.entries(raw)) {
    if (typeof value !== "function") guarded[name] = value
    else if (PASS_THROUGH.has(name)) guarded[name] = value
    else guarded[name] = blockedStub(name, engine)
  }

  guarded.connectBrowserSkill = async function connectBrowserSkill(...args) {
    if (engine !== "connected") throw engineRefusal(engine)
    if (await bridge.isStopped()) throw new BrowserUserStoppedError()
    let rawSession
    try {
      rawSession = await raw.connectBrowserSkill(...args)
    } catch (error) {
      await bridge.emit("connect_failed", undefined, { reason: error?.code ?? error?.name })
      if (NOT_CONNECTED_CODES.has(error?.code)) throw new BrowserNotConnectedError(undefined, { cause: error })
      throw error
    }
    const session = guardSession(rawSession, { bridge, host })
    rawSessions.set(session, rawSession)
    await bridge.emit("started", rawSession?.sessionId)
    return session
  }

  if (typeof raw.bskSnapshot === "function") {
    guarded.bskSnapshot = async function bskSnapshot(session, ...rest) {
      return await raw.bskSnapshot(rawSessions.get(session) ?? session, ...rest)
    }
  }

  return guarded
}
