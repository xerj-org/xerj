#!/usr/bin/env python3
"""Assert that every element of /osd sits on the 32 px matrix.

Usage: check_osd_grid.py URL [WIDTH ...]   (default widths 1440 390)
Needs Python Playwright and a Chromium/Chrome (CHROME env var, default
/usr/bin/google-chrome). Exits 1 on any failure.
"""
import asyncio, json, os, pathlib, sys
from playwright.async_api import async_playwright

JS = (pathlib.Path(__file__).with_name('osd_grid_check.js')).read_text()

async def main(url, widths):
    ok = True
    async with async_playwright() as p:
        b = await p.chromium.launch(executable_path=os.environ.get('CHROME', '/usr/bin/google-chrome'), args=['--no-sandbox'])
        for w in widths:
            pg = await b.new_page(viewport={'width': w, 'height': 900})
            await pg.goto(url, wait_until='networkidle')
            await pg.evaluate('document.fonts.ready.then(() => 1)')
            await pg.wait_for_timeout(300)
            r = await pg.evaluate(JS)
            bx, bl = r['boxes'], r['baselines']
            print(f"{w}px  cells={r['cells']}  boxes {bx['pass']}/{bx['total']}  baselines {bl['pass']}/{bl['total']}  unowned text {len(r['unownedText'])}")
            for f in r['fails'][:60]:
                print('   FAIL', f['kind'], f['el'], '->', f['bad'])
            for t in r['unownedText'][:20]:
                print('   UNOWNED', t)
            if r['fails'] or r['unownedText']:
                ok = False
            await pg.close()
        await b.close()
    return ok

if __name__ == '__main__':
    url = sys.argv[1]
    widths = [int(x) for x in sys.argv[2:]] or [1440, 390]
    sys.exit(0 if asyncio.run(main(url, widths)) else 1)
