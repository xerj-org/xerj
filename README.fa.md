<div dir="rtl" align="right">

# XERJ

<p dir="rtl" align="right">
<strong>XERJ: موتور جست‌وجو برای عامل‌های هوش مصنوعی (<bdi>AI Agents</bdi>).</strong>
رویکردی نوین برای جست‌وجوی داده‌ها توسط هوش مصنوعی؛ یک فایل اجرایی محلی مبتنی بر <bdi>Rust</bdi> که کدها، مستندات، مشخصات فنی و گزارش‌های سیستم (<bdi>Logs</bdi>) شما را ایندکس می‌کند. همچنین با استفاده از بسته‌های <bdi>Corpus Hub</bdi>، امکان دسترسی به مجموعه‌داده‌های آماده را فراهم می‌کند و نتایج جست‌وجو را همراه با ارجاعات و منابع (<bdi>Citations</bdi>) ارائه می‌دهد.
</p>

<p dir="rtl" align="right">
<strong>موتور جست‌وجوی <bdi>Local-first</bdi> و سرور <bdi>MCP</bdi> برای عامل‌های هوش مصنوعی.</strong>
یک فایل اجرایی مبتنی بر <bdi>Rust</bdi> که کدها، مستندات، گزارش‌های سیستم، فایل‌های <bdi>JSONL</bdi> و <bdi>CSV</bdi> و اسناد <bdi>PDF</bdi> را ایندکس می‌کند و قابلیت‌های جست‌وجوی متنی <bdi>BM25</bdi>، جست‌وجوی برداری (<bdi>Vector Search</bdi>)، جست‌وجوی ترکیبی (<bdi>Hybrid Search</bdi>)، جست‌وجوی کد و حافظه عامل‌های هوش مصنوعی را ارائه می‌دهد. تمام این قابلیت‌ها از طریق یک <bdi>API</bdi> سازگار با <bdi>Elasticsearch</bdi> در دسترس هستند.
</p>

<p dir="rtl" align="right">
XERJ با ابزارهایی مانند <bdi>Claude Code</bdi>، <bdi>Cursor</bdi> و هر کلاینت سازگار با <bdi>MCP</bdi> کار می‌کند.
</p>

</div>

<div align="center">

<a href="https://trendshift.io/repositories/116361?utm_source=repository-badge&amp;utm_medium=badge&amp;utm_campaign=badge-repository-116361" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/repositories/116361" alt="xerj-org%2Fxerj | Trendshift" width="250" height="55"/></a>
<a href="https://trendshift.io/repositories/116361#achievements" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/116361/daily?language=Rust" alt="XERJ | #1 Rust Repository Of The Day | Trendshift" width="250" height="55"/></a>
<a href="https://trendshift.io/repositories/116361#achievements" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/116361/weekly?language=Rust" alt="XERJ | #1 Rust Repository Of The Week | Trendshift" width="250" height="55"/></a>
</div>

[English](./README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja-JP.md) | [العربية](./README.ar.md) | [Español](./README.es.md) | [한국어](./README.ko.md) | [فارسی](./README.fa.md)

<div align="center">
  
[![CI](https://img.shields.io/github/actions/workflow/status/xerj-org/xerj/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/xerj-org/xerj/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/xerj-org/xerj?style=flat-square&label=release)](https://github.com/xerj-org/xerj/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square)](./LICENSE)
[![Stars](https://img.shields.io/github/stars/xerj-org/xerj?style=flat-square)](https://github.com/xerj-org/xerj/stargazers)
[![Rust](https://img.shields.io/badge/built%20with-Rust-000000?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![Discussions](https://img.shields.io/github/discussions/xerj-org/xerj?style=flat-square&label=discussions)](https://github.com/xerj-org/xerj/discussions)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?style=flat-square&logo=discord&logoColor=white)](https://discord.com/invite/yetXFusXfs)
[![X](https://img.shields.io/badge/X-%40xerj__org-000000?style=flat-square&logo=x&logoColor=white)](https://x.com/xerj_org)

[![Reference coding](https://img.shields.io/badge/reference%20coding-2.7%C3%97%20fewer%20tokens-ff8c00?style=flat-square)](https://xerj.org/case-studies/reference-coding.html)
[![Autoindex](https://img.shields.io/badge/autoindex-zero%20config-ff8c00?style=flat-square)](https://xerj.org/docs/cli.html)
[![Search](https://img.shields.io/badge/search-BM25%20%2B%20kNN%20%2B%20hybrid-8957e5?style=flat-square)](https://xerj.org/docs/queries.html)
[![Agent memory](https://img.shields.io/badge/agent%20memory-%2F__memory-00b3a4?style=flat-square)](https://xerj.org/docs/recipes/agentic-memory.html)
[![MCP](https://img.shields.io/badge/MCP-native-1f6feb?style=flat-square)](https://xerj.org/llms.txt)
[![llms.txt](https://img.shields.io/badge/llms.txt-agent%20ready-6f42c1?style=flat-square)](https://xerj.org/llms.txt)
[![ES-YAML conformance](https://img.shields.io/badge/ES--YAML%20conformance-1444%2F1454-brightgreen?style=flat-square)](https://xerj.org/benchmarks)
[![Single static binary](https://img.shields.io/badge/single%20static%20binary-no%20JVM-orange?style=flat-square)](https://xerj.org/docs/install.html)
[![Default embedder](https://img.shields.io/badge/default%20embedder-lexical%2C%20offline-teal?style=flat-square)](https://xerj.org/docs/vectors.html)
[![Neural](https://img.shields.io/badge/neural%20embedder-opt--in%2C%20downloads%20\~90%20MB-9c6ade?style=flat-square)](https://xerj.org/docs/vectors.html)

</div>

<p dir="rtl" align="right">
<bdi>XERJ</bdi> یک موتور جست‌وجوی محلی مبتنی بر هوش مصنوعی است که اعتماد جامعه را جلب کرده و به‌صورت خودکار هر پوشه‌ای را ایندکس می‌کند. به این ترتیب، عامل برنامه‌نویسی شما دیگر برای خواندن فایل‌ها به‌صورت تک‌به‌تک، توکن‌های زیادی مصرف نمی‌کند و به‌جای آن، کد دقیق موردنیازش را مستقیماً پیدا می‌کند.
</p>

<p dir="rtl" align="right">
مهم‌ترین کاربرد <bdi>XERJ</bdi> و برجسته‌ترین مزیت آن، <bdi>Reference Coding</bdi> است: کافی است یک وظیفه را به عامل هوش مصنوعی بسپارید تا مخازن متن‌باز مرتبط با آن را دانلود و ایندکس کند و پیش از نوشتن کد خودش، از راهکارهایی که قبلاً برای حل همان مسئله استفاده شده‌اند بهره ببرد.
</p>

<p dir="rtl" align="right">
در یک مطالعه کنترل‌شده، این روش توانست تعداد توکن‌های خروجی عامل برنامه‌نویسی را ۲٫۷ برابر کاهش دهد، در حالی که نرخ حل مسئله همچنان در سطح ۱۶ از ۱۶ باقی ماند
(<a href="https://xerj.org/case-studies/reference-coding.html"><bdi>مطالعه موردی</bdi></a>).
همچنین کاربران در استفاده روزمره، از کاهش تقریبی ۵ برابری گزارش داده‌اند
(<a href="./user-feedback/11-reference-coding-field-reports/2026-08-11-token-savings-reports.md"><bdi>گزارش‌های کاربران</bdi></a>).
</p>

<p dir="rtl" align="right">
این قابلیت به‌اندازه‌ای مؤثر است که یک مدل کوچک‌تر و ارزان‌تر، با دسترسی به این روش، می‌تواند عملکرد برنامه‌نویسی بهتری از یک مدل گران‌تر داشته باشد که صرفاً به حافظه خودش متکی است؛ در نتیجه، زمان کمتری را صرف چرخه‌های اصلاح و رفع خطا خواهید کرد.
</p>

<p dir="rtl" align="right">
همین حالا با نصب یک‌مرحله‌ای و تنها با یک دستور متنی آن را امتحان کنید:
</p>

<pre dir="ltr"><code>Install XERJ (docs: https://xerj.org/llms.txt), index this project's sources, and set up
reference coding: clone and index the open-source repos closest to what we're building,
and search how they solved a problem before writing code.</code></pre>

<p dir="rtl" align="right">
یا خودتان آن را اجرا کنید:
</p>

<pre dir="ltr"><code>curl -fsSL https://xerj.org/get | sh
xerj --insecure --data-dir ~/xerj-data &
xerj init                                        # wire XERJ into Claude Code/Cursor (MCP + skill), one command
xerj autoindex ~/my-project
xerj search "how do we rotate the WAL segment"   # ranked passages, file:line
xerj def "euler_to_rotationmatrix"               # go-to-definition: file:line + signature
xerj gain                                        # what it did for you — counted, not estimated</code></pre>

<p dir="rtl" align="right">
برای اجرای هم‌زمان چند شناسه یا پرسش، از دستور <bdi><code>xerj batch-search --input queries.json</code></bdi> یا <bdi><code>MCP</code></bdi> و متد <bdi><code>xerj_batch_search</code></bdi> استفاده کنید:
<a href="docs/recipes/batch-search.md"><bdi>نتایج مرتب‌شده، ارجاعات و خطاهای هر مورد</bdi></a>.
</p>

<p dir="rtl" align="right">
پوشه داده را خارج از هر پوشه‌ای قرار دهید که قصد دارید با <bdi><code>autoindex</code></bdi> ایندکس کنید. مسیر <bdi><code>data/</code></bdi> در فهرست مسیرهای نادیده‌گرفته‌شده پیش‌فرض قرار ندارد؛ بنابراین اگر پوشه داده را با استفاده از <bdi><code>--data-dir ./data</code></bdi> داخل درخت پوشه‌ای قرار دهید که بعداً آن را ایندکس می‌کنید، نود اطلاعات ذخیره‌سازی خودش را نیز ایندکس خواهد کرد.
</p>

<p dir="rtl" align="right">
هنگام راه‌اندازی، مقدار <bdi><code>Data dir</code></bdi> مسیر مطلق نهایی را نمایش می‌دهد، <bdi><code>Data source</code></bdi> مشخص می‌کند این مسیر از کجا انتخاب شده است (پارامتر خط فرمان، فایل پیکربندی انتخاب‌شده یا مقدار پیش‌فرض) و <bdi><code>Data status</code></bdi> نشان می‌دهد که آیا این اجرا پوشه را ایجاد کرده است یا خیر.
</p>

<p dir="rtl" align="right">
استفاده از یک پوشه جدید ممکن است باعث شود ایندکس‌ها یا حساب مالک موجود در یک نود قدیمی، دیگر در دسترس به نظر نرسند. برای دسترسی مجدد، برنامه را با مسیر قبلی <bdi><code>--data-dir</code></bdi> یا فایل پیکربندی قبلی از طریق <bdi><code>--config</code></bdi> راه‌اندازی کنید. <bdi><code>XERJ</code></bdi> پوشه‌های داده دیگر را به‌صورت خودکار شناسایی یا مهاجرت نمی‌دهد.
</p>

<p dir="rtl" align="right">
راهنمای نصب در ویندوز و نصب دستیِ تأییدشده در بخش <a href="#install-by-hand"><bdi><code>Install by hand</code></bdi></a> قرار دارد.
</p>

<p dir="rtl" align="right">
پارامتر <bdi><code>--insecure</code></bdi> یک مرز امنیتی مهم را تغییر می‌دهد و صرفاً یک گزینه تشریفاتی نیست. این پارامتر <bdi><code>TLS</code></bdi> و احراز هویت مبتنی بر <bdi><code>API Key</code></bdi> را غیرفعال می‌کند؛ بنابراین هر کلاینتی که بتواند به پورت دسترسی پیدا کند، دسترسی <bdi><code>Superuser</code></bdi> خواهد داشت و می‌تواند تمام ایندکس‌ها، از جمله تمام فضاهای نام <bdi><code>/_memory</code></bdi> را بخواند یا حذف کند.
</p>

<p dir="rtl" align="right">
این حالت برای استفاده یک کاربر روی یک دستگاه توسعه در نظر گرفته شده است: یک نود تازه به‌طور پیش‌فرض فقط به آدرس <bdi><code>127.0.0.1</code></bdi> متصل می‌شود و اگر بخواهید آن را بدون رمزنگاری روی رابط شبکه دیگری در دسترس قرار دهید، برنامه تا زمانی که صراحتاً اجازه این کار را صادر نکنید، راه‌اندازی نخواهد شد.
</p>

<p dir="rtl" align="right">
مدل امنیتی کامل در فایل <a href="./docs/SECURITY_MODEL.md"><bdi><code>docs/SECURITY_MODEL.md</code></bdi></a> توضیح داده شده است.
</p>