#!/usr/bin/env python3
"""preview-local.py: render an inline HTML page in four views (light/dark x reply width/390 px),
with all network blocked, and report height, horizontal overflow and console errors.

MIT License (c) 2026 Indo Yoon.

Optional helper for hosts where Playwright + a Chromium binary are available locally.
If they are not, preview through the harness browser skill instead (see SKILL.md).

Usage: python3 preview-local.py <page.html> <out_dir> [--reply-width 720] [--no-js] [--chromium /usr/bin/chromium]
Prints a JSON report; exit 0 if every view is clean, 1 otherwise.
"""
import argparse
import json
import os
import sys
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("page")
    ap.add_argument("out_dir")
    ap.add_argument("--reply-width", type=int, default=720)
    ap.add_argument("--no-js", action="store_true", help="render with JavaScript disabled")
    ap.add_argument("--chromium", default=os.environ.get("CHROMIUM_PATH"))
    a = ap.parse_args()
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        print(json.dumps({"error": "playwright not installed; use the browser skill to preview"}))
        return 2

    url = Path(a.page).resolve().as_uri()
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    report, ok = [], True
    with sync_playwright() as p:
        launch = {"executable_path": a.chromium} if a.chromium else {}
        browser = p.chromium.launch(**launch)
        for scheme in ("light", "dark"):
            for label, width in (("reply", a.reply_width), ("phone", 390)):
                ctx = browser.new_context(
                    viewport={"width": width, "height": 800},
                    color_scheme=scheme,
                    java_script_enabled=not a.no_js,
                    offline=True,
                )
                blocked = []

                def route(r, blocked=blocked):
                    if r.request.url.startswith("file:") or r.request.url.startswith("data:"):
                        return r.continue_()
                    blocked.append(r.request.url)
                    return r.abort()

                ctx.route("**/*", route)
                page = ctx.new_page()
                errors = []
                page.on("console", lambda m, errors=errors: errors.append(m.text) if m.type == "error" else None)
                page.on("pageerror", lambda e, errors=errors: errors.append(str(e)))
                page.goto(url)
                page.wait_for_load_state("load")
                m = page.evaluate(
                    """() => {
                      const d = document.documentElement;
                      const clipped = [...document.querySelectorAll('body *')].filter(el => {
                        const s = getComputedStyle(el);
                        return (s.overflow === 'hidden' || s.overflowX === 'hidden') && el.scrollWidth > el.clientWidth + 1;
                      }).length;
                      return { height: Math.ceil(document.body.getBoundingClientRect().height),
                               overflowX: d.scrollWidth > d.clientWidth + 1, clipped };
                    }"""
                )
                shot = out / f"{Path(a.page).stem}-{scheme}-{label}{'-nojs' if a.no_js else ''}.png"
                page.screenshot(path=str(shot), full_page=True)
                view = {"scheme": scheme, "width": width, "screenshot": str(shot), **m,
                        "console_errors": errors, "blocked_requests": blocked}
                if m["overflowX"] or m["clipped"] or errors or blocked:
                    ok = False
                report.append(view)
                ctx.close()
        browser.close()
    print(json.dumps(report, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
