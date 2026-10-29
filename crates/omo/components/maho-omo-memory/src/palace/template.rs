use std::sync::LazyLock;

use super::client_script::palace_client_script;
use super::styles::PALACE_STYLES;

pub const PALACE_DATA_PLACEHOLDER: &str = "<!--OMO_PALACE_DATA-->";
pub const PALACE_DATA_ELEMENT_ID: &str = "omo-palace-data";

pub const PALACE_PEOPLE_TAB: &str =
    r#"<button class="tab" data-tab="people" type="button">People</button>"#;
pub const PALACE_PEOPLE_PANEL: &str = r#"<section id="panel-people" hidden></section>"#;

const PALACE_TEMPLATE_SOURCE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Memory Palace</title>
<style>
__PALACE_STYLES__</style>
</head>
<body>
<div class="shell">
  <header class="header">
    <h1>Memory Palace</h1>
    <div class="meta">
      <div>identity <strong id="meta-identity"></strong></div>
      <div>HEAD <strong id="meta-head"></strong></div>
      <div>recompiled <strong id="meta-recompiled"></strong></div>
    </div>
  </header>
  <nav class="tabs" id="tabs">
    <button class="tab active" data-tab="core" type="button">Core</button>
    <button class="tab" data-tab="external" type="button">External</button>
    <button class="tab" data-tab="history" type="button">History</button>
    <button class="tab" data-tab="reflection" type="button">Reflection</button>
    <button class="tab" data-tab="people" type="button">People</button>
  </nav>
  <main class="main">
    <section id="panel-core"></section>
    <section id="panel-external" hidden></section>
    <section id="panel-history" hidden></section>
    <section id="panel-reflection" hidden></section>
    <section id="panel-people" hidden></section>
  </main>
  <footer class="footer" id="footer"></footer>
</div>
<script type="application/json" id="omo-palace-data"><!--OMO_PALACE_DATA--></script>
<script>
__PALACE_CLIENT_SCRIPT__</script>
</body>
</html>
"#;

pub static PALACE_TEMPLATE: LazyLock<String> = LazyLock::new(|| {
    PALACE_TEMPLATE_SOURCE
        .replace("__PALACE_STYLES__", PALACE_STYLES)
        .replace(
            "__PALACE_CLIENT_SCRIPT__",
            &palace_client_script(PALACE_DATA_ELEMENT_ID),
        )
});
