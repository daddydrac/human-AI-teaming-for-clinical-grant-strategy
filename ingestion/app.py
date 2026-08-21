import asyncio
import ipaddress
import os
import socket
from contextlib import asynccontextmanager
from urllib.parse import urlparse

from fastapi import FastAPI, HTTPException
from markdownify import markdownify
from playwright.async_api import Browser, Error as PlaywrightError, Route, async_playwright
from pydantic import BaseModel


NAVIGATION_TIMEOUT_MS = max(5_000, min(120_000, int(os.getenv("INGESTION_NAVIGATION_TIMEOUT_MS", "45000"))))
MAX_OUTPUT_BYTES = max(64 * 1024, min(64 * 1024 * 1024, int(os.getenv("INGESTION_MAX_OUTPUT_BYTES", "8388608"))))
RENDER_SLOTS = asyncio.Semaphore(max(1, min(8, int(os.getenv("INGESTION_MAX_CONCURRENCY", "2")))))


class ExtractRequest(BaseModel):
    url: str


class Runtime:
    browser: Browser | None = None
    playwright = None


runtime = Runtime()


def _public_ip(value: str) -> bool:
    ip = ipaddress.ip_address(value)
    return not (ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_multicast or ip.is_reserved or ip.is_unspecified)


async def _validate_public_url(url: str) -> None:
    parsed = urlparse(url)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("only public http/https URLs are supported")
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    loop = asyncio.get_running_loop()
    addresses = await loop.getaddrinfo(parsed.hostname, port, type=socket.SOCK_STREAM)
    if not addresses or any(not _public_ip(item[4][0]) for item in addresses):
        raise ValueError("URL resolves to a private, local, reserved, or unreachable destination")


@asynccontextmanager
async def lifespan(_: FastAPI):
    runtime.playwright = await async_playwright().start()
    runtime.browser = await runtime.playwright.chromium.launch(
        headless=True,
        args=["--no-sandbox", "--disable-dev-shm-usage", "--disable-background-networking"],
    )
    try:
        yield
    finally:
        if runtime.browser is not None:
            await runtime.browser.close()
        if runtime.playwright is not None:
            await runtime.playwright.stop()


app = FastAPI(title="Grant HTML Ingestion", version="1.0.0", lifespan=lifespan)


@app.get("/health")
async def health():
    return {"status": "ok", "browser_ready": runtime.browser is not None and runtime.browser.is_connected()}


@app.post("/extract")
async def extract(request: ExtractRequest):
    try:
        await _validate_public_url(request.url)
    except (ValueError, OSError, socket.gaierror) as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    if runtime.browser is None:
        raise HTTPException(status_code=503, detail="browser is not ready")

    await RENDER_SLOTS.acquire()
    context = None
    try:
        context = await runtime.browser.new_context(
            java_script_enabled=True,
            ignore_https_errors=False,
            user_agent=os.getenv("RESEARCH_USER_AGENT", "ClinicalGrantWorkbench/0.8.0"),
        )
        page = await context.new_page()

        async def bounded_route(route: Route):
            resource_type = route.request.resource_type
            if resource_type in {"image", "media", "font"}:
                await route.abort()
                return
            try:
                await _validate_public_url(route.request.url)
            except (ValueError, OSError, socket.gaierror):
                await route.abort()
                return
            await route.continue_()

        await page.route("**/*", bounded_route)
        response = await page.goto(request.url, wait_until="domcontentloaded", timeout=NAVIGATION_TIMEOUT_MS)
        if response is None:
            raise HTTPException(status_code=502, detail="the page returned no navigation response")
        if response.status >= 400:
            raise HTTPException(status_code=502, detail=f"the page returned HTTP {response.status}")
        try:
            await page.wait_for_load_state("networkidle", timeout=min(7_500, NAVIGATION_TIMEOUT_MS))
        except Exception:
            # Many modern sites retain long-lived network connections. The DOM
            # content loaded state remains a deterministic bounded fallback.
            pass
        title = (await page.title()).strip() or request.url
        final_url = page.url
        html = await page.evaluate("""() => {
            const root = document.querySelector('main, article, [role="main"]') || document.body;
            if (!root) return '';
            const copy = root.cloneNode(true);
            copy.querySelectorAll('script,style,noscript,svg,canvas,form,button,nav,footer').forEach(n => n.remove());
            return copy.outerHTML;
        }""")
        text = markdownify(html or "", heading_style="ATX", bullets="-").replace("\r\n", "\n").replace("\r", "\n").strip()
        if not text:
            raise HTTPException(status_code=422, detail="the rendered page contains no readable main content")
        size = len(text.encode("utf-8"))
        if size > MAX_OUTPUT_BYTES:
            raise HTTPException(status_code=413, detail=f"rendered Markdown exceeds the configured {MAX_OUTPUT_BYTES}-byte limit")
        return {"title": title, "url": final_url, "text": text, "status": response.status, "content_type": "text/markdown"}
    except HTTPException:
        raise
    except PlaywrightError as exc:
        raise HTTPException(status_code=502, detail=f"browser could not render the page: {exc}") from exc
    finally:
        if context is not None:
            await context.close()
        RENDER_SLOTS.release()
