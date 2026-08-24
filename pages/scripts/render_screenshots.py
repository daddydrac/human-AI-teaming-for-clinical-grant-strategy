#!/usr/bin/env python3
"""Render the documentation's fictional interface walkthroughs."""
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets" / "mockups" / "interface.html"
OUTPUT = ROOT / "assets" / "images"


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        page = browser.new_page(viewport={"width": 1440, "height": 850}, device_scale_factor=1)
        page.goto(SOURCE.as_uri(), wait_until="networkidle")
        for frame in page.locator("[data-shot]").all():
            name = frame.get_attribute("data-shot")
            if not name:
                raise RuntimeError("every screenshot frame requires data-shot")
            frame.screenshot(path=str(OUTPUT / f"{name}.png"))
        browser.close()
    print(f"Rendered {len(list(OUTPUT.glob('*.png')))} walkthrough screenshots into {OUTPUT}")


if __name__ == "__main__":
    main()
