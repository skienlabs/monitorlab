#!/usr/bin/env python3
"""
UI regression walk: signs in, opens every page in light and dark mode, takes screenshots and
fails on any browser console error (other than the expected 401 before sign-in).

  pip install playwright && python -m playwright install chromium
  python3 tests/ui_walk.py --hub https://127.0.0.1:8443 --email admin@test.local --password '...' --out shots/
"""
import argparse, asyncio, os, sys
from playwright.async_api import async_playwright

ap = argparse.ArgumentParser()
ap.add_argument("--hub", required=True)
ap.add_argument("--email", required=True)
ap.add_argument("--password", required=True)
ap.add_argument("--out", default="shots")
ap.add_argument("--chrome", default=None, help="path to a Chrome/Chromium binary (optional)")
ap.add_argument("--container", default=None, help="container name to open (default: first running)")
A = ap.parse_args()
H = A.hub.rstrip("/")
OUT = A.out
os.makedirs(OUT, exist_ok=True)
errors = []

async def main():
    async with async_playwright() as p:
        b = await p.chromium.launch(executable_path=A.chrome, args=["--ignore-certificate-errors"])
        ctx = await b.new_context(viewport={"width": 1440, "height": 900}, ignore_https_errors=True, device_scale_factor=1)
        page = await ctx.new_page()
        page.on("console", lambda m: m.type == "error" and errors.append(f"{page.url}: {m.text}"))
        page.on("pageerror", lambda e: errors.append(f"{page.url}: PAGEERROR {e}"))
        await page.emulate_media(color_scheme="light")
        await page.goto(H + "/")
        await page.wait_for_selector("#email")
        await page.screenshot(path=f"{OUT}/01-login.png")
        await page.fill("#email", A.email); await page.fill("#password", A.password)
        await page.click("button:has-text('Sign in')")
        await page.wait_for_selector(".server-card")
        await page.wait_for_timeout(800)
        await page.screenshot(path=f"{OUT}/02-overview.png")
        sid = await page.evaluate("fetch('/api/servers').then(r => r.json()).then(l => l.find(s => s.is_local).id)")
        cont = A.container or await page.evaluate(f"fetch('/api/servers/{sid}/rpc', {{method:'POST', headers:{{'content-type':'application/json','x-vigil-csrf':'1'}}, body: JSON.stringify({{op:'docker_containers'}})}}).then(r => r.json()).then(l => l.filter(c => c.state === 'running').map(c => c.name).sort()[0])")
        await page.goto(f"{H}/servers/{sid}"); await page.wait_for_selector(".uplot"); await page.wait_for_timeout(1500)
        await page.mouse.move(500, 620); await page.wait_for_timeout(300)
        await page.screenshot(path=f"{OUT}/03-server-overview.png", full_page=True)
        for tab in ["containers", "images", "volumes", "networks", "cleanup"]:
            await page.goto(f"{H}/servers/{sid}?tab={tab}"); await page.wait_for_selector("table, .stack-bar", timeout=15000); await page.wait_for_timeout(900)
            await page.screenshot(path=f"{OUT}/04-{tab}.png", full_page=True)
        await page.goto(f"{H}/servers/{sid}/containers/{cont}?tab=logs"); await page.wait_for_selector(".logs-view .ln"); await page.wait_for_timeout(2500)
        await page.screenshot(path=f"{OUT}/05-logs.png")
        await page.fill(".logs-bar input", "fail"); await page.wait_for_timeout(400)
        await page.screenshot(path=f"{OUT}/05b-logs-search.png")
        await page.goto(f"{H}/servers/{sid}/containers/{cont}?tab=terminal"); await page.wait_for_selector(".xterm-screen"); await page.wait_for_timeout(1200)
        await page.keyboard.type("echo UI_OK && ls /\n"); await page.wait_for_timeout(1500)
        term_text = await page.inner_text(".xterm-rows")
        if "UI_OK" not in term_text: errors.append("terminal did not echo UI_OK")
        await page.screenshot(path=f"{OUT}/06-terminal.png")
        await page.goto(f"{H}/servers/{sid}/containers/{cont}?tab=stats"); await page.wait_for_timeout(6000)
        await page.screenshot(path=f"{OUT}/07-stats.png")
        await page.goto(f"{H}/servers/{sid}/containers/{cont}?tab=inspect"); await page.wait_for_selector(".kv"); await page.wait_for_timeout(500)
        await page.screenshot(path=f"{OUT}/08-inspect.png", full_page=True)
        await page.goto(f"{H}/admin/servers"); await page.wait_for_selector("table"); await page.click("button:has-text('Add server')")
        await page.fill(".dialog input", "prod-iis-02"); await page.click(".dialog button:has-text('Continue')")
        await page.wait_for_selector(".code-block"); await page.wait_for_timeout(300)
        await page.screenshot(path=f"{OUT}/09-add-server.png")
        await page.click(".dialog .seg button:has-text('Windows')"); await page.wait_for_timeout(200)
        await page.screenshot(path=f"{OUT}/09b-add-server-windows.png")
        await page.click(".dialog button:has-text('Done')")
        # remove the server created by this walk
        await page.evaluate("fetch('/api/servers').then(r => r.json()).then(l => Promise.all(l.filter(s => s.name === 'prod-iis-02').map(s => fetch('/api/servers/' + s.id, {method: 'DELETE', headers: {'x-vigil-csrf': '1'}}))))")
        await page.goto(f"{H}/admin/users"); await page.wait_for_selector("table"); await page.click("button:has-text('Add user')"); await page.wait_for_timeout(300)
        await page.screenshot(path=f"{OUT}/10-users-add.png")
        await page.keyboard.press("Escape")
        await page.goto(f"{H}/admin/roles"); await page.wait_for_selector(".perm-group, .card"); await page.wait_for_timeout(300)
        await page.screenshot(path=f"{OUT}/11-roles.png", full_page=True)
        await page.goto(f"{H}/admin/audit"); await page.wait_for_selector("table"); await page.wait_for_timeout(300)
        await page.screenshot(path=f"{OUT}/12-audit.png")
        await page.goto(f"{H}/alerts"); await page.wait_for_selector(".page-head"); await page.wait_for_timeout(600)
        await page.screenshot(path=f"{OUT}/16-alerts.png")
        await page.goto(f"{H}/admin/alerts"); await page.wait_for_selector("text=Email server (SMTP)"); await page.wait_for_timeout(600)
        await page.screenshot(path=f"{OUT}/17-alert-settings.png", full_page=True)
        await page.click("button:has-text('Add channel')"); await page.wait_for_selector(".dialog"); await page.wait_for_timeout(300)
        await page.screenshot(path=f"{OUT}/18-add-channel.png")
        await page.keyboard.press("Escape")
        await page.goto(f"{H}/account"); await page.wait_for_selector("text=Change password")
        # dark mode
        await page.evaluate("localStorage.setItem('vigil.theme','dark')")
        await page.goto(f"{H}/servers/{sid}"); await page.wait_for_selector(".uplot"); await page.wait_for_timeout(1500)
        await page.screenshot(path=f"{OUT}/13-dark-server.png")
        await page.goto(f"{H}/servers/{sid}/containers/{cont}?tab=logs"); await page.wait_for_selector(".logs-view .ln"); await page.wait_for_timeout(1500)
        await page.screenshot(path=f"{OUT}/14-dark-logs.png")
        await page.goto(f"{H}/"); await page.wait_for_selector(".server-card"); await page.wait_for_timeout(500)
        await page.screenshot(path=f"{OUT}/15-dark-overview.png")
        # narrow viewport
        await page.set_viewport_size({"width": 420, "height": 860})
        await page.goto(f"{H}/"); await page.wait_for_selector(".server-card"); await page.wait_for_timeout(500)
        await page.screenshot(path=f"{OUT}/19-mobile.png")
        wide = await page.evaluate("document.documentElement.scrollWidth > window.innerWidth + 2")
        if wide: errors.append("horizontal overflow on narrow viewport")
        await b.close()
    real = [e for e in errors if "401 (Unauthorized)" not in e]
    print("screenshots in", OUT)
    print("problems:", len(real))
    for e in real[:30]: print(" ", e[:300])
    sys.exit(1 if real else 0)

asyncio.run(main())
