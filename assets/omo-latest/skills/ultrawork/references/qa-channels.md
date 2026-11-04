# QA Channels and Execution Verification

This reference defines execution protocols for manual, protocol-level, browser-based, desktop GUI, and visual interface verification across specialized operational channels.

---

## 1. Protocol-Level HTTP Verification

When task deliverables involve HTTP endpoints, REST services, WebSocket connections, or RPC handlers, verification must test the genuine network protocol layer rather than calling internal controller functions:
- **Direct Wire Testing**: Exercise endpoints using command-line utilities (such as `curl`) or native HTTP fetch calls against active local listeners.
- **Header and Status Assertions**: Explicitly validate HTTP status codes, content-type headers, cache controls, authentication headers, and response payloads. Do not assume success based on an absence of connection errors.
- **Payload Schema Validation**: Parse returned JSON or structured responses, confirming required fields, data types, and error structures match the API specification.
- **Edge Conditions**: Test missing query parameters, malformed bodies, invalid authentication tokens, and boundary payloads to verify proper error status codes (such as 400, 401, 403, 404, or 422) rather than unhandled 500 server crashes.

---

## 2. Browser Automation and Session Preservation

For web applications and client-side interfaces requiring real browser verification:
- **Real Browser Sessions**: Automate genuine browser engines (such as Chromium via headless browser APIs) to interact with rendered web surfaces.
- **Session and Cookie Preservation**: When testing environments where the user has existing authenticated sessions, profiles, or cookies, preserve local storage, session storage, and cookie jars. Do not wipe or reset existing authentication state unless testing a clean unauthenticated onboarding flow is the explicit goal.
- **State Transitions**: Test interactive states by driving realistic user actions (clicking buttons, filling forms, navigating routes) and asserting observable DOM changes rather than merely asserting initial HTML markup.

---

## 3. Desktop GUI Automation

For native desktop applications (such as macOS or Linux desktop software):
- **Native Automation Drivers**: Drive native interface components using platform automation tools and operating system accessibility hooks.
- **Control Interactions**: Exercise genuine menu bars, window controls, dialog buttons, and keyboard shortcuts.
- **State Observability**: Verify window presence, active focus, text updates, and dialog responses rather than inspecting static resource files.

---

## 4. Manual Proof Specification

When automated assertions cannot fully cover a scenario and manual verification proofs are required:
- **Defined Inputs and Commands**: Specify the exact command invocation, environment variables, and input data required to reproduce the scenario.
- **Binary Pass/Fail Criteria**: Define unambiguous observable outcomes that distinguish success from failure. Ambiguous guidelines ("check if it looks good") are prohibited.
- **Execution Receipts**: Record the exact output, exit codes, and timestamps produced by the proof execution.

---

## 5. Non-GUI Evidence Formats

For backend services, command-line utilities, daemons, and database layers that lack graphical interfaces:
- **Structured CLI Dumps**: Capture command output formatted as JSON, YAML, or tab-delimited text.
- **Schema and Data Diffs**: Capture unified diffs of generated files, schema migrations, or configuration files before and after the change.
- **Database Records**: Query and display database table rows, transaction receipts, or state tables confirming that persistent state was mutated correctly.

---

## 6. Visual Deliverables and Viewport Matrix

For visual web or document interfaces, changes must be captured and inspected across multiple viewports:
- **Paired Viewport Captures**: Capture rendered screenshots after each visual change at both a narrow mobile width (390px, representative of modern smartphones) and a wide desktop width (1440px or 1920px).
- **Responsive Inspection**: Confirm that content reflows cleanly without horizontal scrolling at mobile widths, text does not truncate awkwardly, navigation menus collapse appropriately, and wide layouts utilize space effectively.
- **Before and After Comparison**: For modifications to existing visual surfaces, capture reference screenshots before editing and compare them side by side with post-edit captures.

---

## 7. Event-Driven Completion Notifications

When initiating long-running test suites, compilation jobs, or container deployments:
- **Push Notification Registration**: Arrange for a pushed completion event (via `tool.monitor`) before proceeding to dependent tasks.
- **No Idle Polling**: Never execute repeated polling calls or sleeping loops while waiting for long-running verification jobs to finish.
