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