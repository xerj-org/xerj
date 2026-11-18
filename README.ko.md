# XERJ

**AI 에이전트를 위한 로컬 우선(local-first) 검색 엔진이자 MCP 서버.** Rust 바이너리 하나가 코드, 문서,
로그, JSONL/CSV, PDF를 색인해 BM25 전문(full-text) 검색, 벡터 검색, 하이브리드 검색, 코드 검색,
에이전트 메모리를 제공하며, Elasticsearch 호환 API 뒤에서 동작합니다. Claude Code, Cursor 및 모든
MCP 클라이언트에서 사용할 수 있습니다.

<a href="https://trendshift.io/repositories/116361?utm_source=repository-badge&amp;utm_medium=badge&amp;utm_campaign=badge-repository-116361" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/repositories/116361" alt="xerj-org%2Fxerj | Trendshift" width="250" height="55"/></a>

[English](./README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja-JP.md) | [العربية](./README.ar.md) | [Español](./README.es.md) | [한국어](./README.ko.md)

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
[![Neural](https://img.shields.io/badge/neural%20embedder-opt--in%2C%20downloads%20~90%20MB-9c8ade?style=flat-square)](https://xerj.org/docs/vectors.html)

XERJ는 커뮤니티의 신뢰를 받는 로컬 AI 검색 엔진으로, 어떤 폴더든 자동으로 색인합니다. 덕분에
코딩 에이전트는 파일을 하나씩 읽으며 토큰을 낭비하는 대신, 정확히 필요한 코드를 바로 가져올 수
있습니다. 레퍼런스 코딩(reference coding)이 대표적인 사용 사례이자 가장 확실한 승리입니다: 에이전트에게
작업을 지시하면 그와 가장 가까운 오픈소스 저장소들을 내려받아 색인하고, 자기 코드를 작성하기 전에
그 문제를 다른 사람들이 어떻게 해결했는지 재활용합니다. 제어된 실험에서 코딩 에이전트의 출력
토큰을 동일한 16/16 해결률로 2.7배 줄였고([사례 연구](https://xerj.org/case-studies/reference-coding.html)),
사용자들은 실무에서 대략 5배 절감을 보고합니다
([실사용 보고서](./user-feedback/11-reference-coding-field-reports/2026-08-11-token-savings-reports.md)).
덕분에 더 작고 저렴한 모델이 기억에 의존하는 비싼 모델보다 더 잘 코딩할 수 있고, 여러분은 수정
루프에 쓰는 시간도 줄어듭니다. 프롬프트 하나로 바로 설치해 보세요:

```text
Install XERJ (docs: https://xerj.org/llms.txt), index this project's sources, and set up
reference coding: clone and index the open-source repos closest to what we're building,
and search how they solved a problem before writing code.
```

직접 실행할 수도 있습니다:

```sh
curl -fsSL https://xerj.org/get | sh
xerj --insecure --data-dir ~/xerj-data &
xerj init                                        # Claude Code/Cursor에 XERJ 연동 (MCP + skill), 명령 한 번
xerj autoindex ~/my-project
xerj search "how do we rotate the WAL segment"   # 순위가 매겨진 구절, file:line
xerj def "euler_to_rotationmatrix"               # 정의로 이동: file:line + 시그니처
xerj gain                                        # 무엇을 해줬는지 — 추정이 아닌 실측
```

데이터 디렉터리는 autoindex할 폴더 밖에 두세요. `data/`는 기본 무시 목록에 없으므로, 색인할
트리 안에 `--data-dir ./data`를 두면 노드가 자기 저장소를 색인하게 됩니다.

Windows 및 수동 검증된 설치는 [수동 설치](#수동-설치)를 참고하세요.

이 `--insecure` 플래그는 형식적인 절차가 아니라 신뢰 경계입니다. TLS와 API 키 인증을 끄므로,
해당 포트에 도달할 수 있는 모든 호출자는 모든 `/_memory` 네임스페이스를 포함해 모든 인덱스를
읽고 삭제할 수 있는 슈퍼유저가 됩니다. 개발 머신의 단일 사용자를 위한 것입니다: 새 노드는
`127.0.0.1`에만 바인딩하며, 다른 인터페이스의 평문 바인딩은 명시적으로 허용하지 않는 한
시작을 거부합니다. 전체 모델은
[docs/SECURITY_MODEL.md](./docs/SECURITY_MODEL.md)에 있습니다.

## 커뮤니티 참여

XERJ에 참여하는 방법은 실행해 보고 짧은 사용 후기를 하나 보내는 것 — 커밋이나 PR 하나 — 입니다.
그 보고서가 회원증이며, 무료 Apache-2.0 바이너리에 대한 프로젝트의 유일한 대가입니다. 덕분에
다음 사용자를 위해 엔진이 계속 개선됩니다.

사람과 AI 에이전트 모두 환영합니다; 의도적으로 에이전트 전용 프로젝트가 아닙니다. 점심시간에
저장소 하나를 색인했든, 에이전트가 남의 코드베이스 세션 중간에 XERJ를 설치했든, 요구는 같습니다:
무엇을 대상으로 삼았는지, 무엇에 사용했는지, 솔직한 평가를 담은 짧은 보고서 하나. 부정적 평가도
좋은 보고서입니다. 뭔가 깨졌다면 그건 별도의 이슈나 수정 사항입니다.

사용 후기 PR은 CLA 면제라 서명 없이 병합됩니다. 어떤 빌드에서든 동작하는 방법은
[`user-feedback/16-agent-field-reports/`](./user-feedback/16-agent-field-reports)에 마크다운
파일 하나만 추가하는 일반 `gh pr create`입니다(디프에 다른 것은 없어야 합니다). 해당 명령이
있는 빌드에서는 `xerj feedback`이 보고서 초안을 작성해 버전, OS, 색인 대상을 채워주고,
`xerj feedback --open-pr`가 PR을 열어줍니다. 내 바이너리에 이 명령이 있는지는 `xerj --help`로
확인하세요.

## 측정된 이유: 레퍼런스 코딩

grep하는 에이전트는 파일 전체를 컨텍스트에 끌어들입니다. 우리 측정에서 한 코퍼스에서는 입력
토큰이 최대 1.06M에 달했고, 결국 그것을 다 읽어야 합니다. XERJ에 질의하는 에이전트는 필요한
구절(passage)을 받습니다. 모델이 암기하지 못한 코드로 엔드투엔드 측정한 결과
([사례 연구](https://xerj.org/case-studies/reference-coding.html): 작업 8개, 언어 4개,
그룹당 16회 실행, 실제 `claude -p` 토큰 카운트):

| | 출력 토큰 | 비용 | 해결 |
|---|---:|---:|---:|
| 기억만으로 | 260,916 | $11.18 | 11/16 |
| grep 기반 에이전트 | 26,477 | $3.27 | 16/16 |
| XERJ | 9,982 | $1.58 | 16/16 |

동일한 해결률에서 grep 대비 출력 토큰 2.7배, 기억만 사용 대비 26배 절감이며, 단일 Java 작업에서는
최대 278배 절감이었습니다. 실제 개발에서 사용자들은 엔드투엔드로 대략 5배의 토큰 절감을
보고합니다
([실사용 보고서](./user-feedback/11-reference-coding-field-reports/2026-08-11-token-savings-reports.md)).
이 가치는 암기 여부에 좌우됩니다: 비공개·내부·니치·학습 컷오프 이후 코드에서 승리하고, 모델이 이미
아는 유명 공개 라이브러리에서는 중립적이거나 불리할 수 있습니다. 정직한 한계는
[사례 연구](https://xerj.org/case-studies/reference-coding.html)에 있습니다.

## 아무 폴더나 넣어 보세요

레퍼런스 코딩도 같은 원시 기능의 한 사용례입니다: `xerj autoindex <folder>`는 에이전트가
코퍼스를 grep하는 대신 알게 만듭니다. 명령 하나로 코드, 문서, 로그, PDF, SQLite, 까다로운 CSV까지
타입이 지정되고 질의 가능한 인덱스로 만들어 검색, RAG, 보안 감사, 에이전트 메모리에 씁니다.
스키마도 파이프라인도 작성할 필요가 없습니다.

```sh
xerj --insecure --data-dir ~/xerj-data &      # 시작
xerj autoindex ~/my-project              # 아무거나 가리키기
```

`--insecure`는 다시 말하지만 TLS 없음, 인증 없음 — 모든 호출자가 슈퍼유저 — 를 뜻하므로, 위와
같은 개발 머신 단일 사용자 경계입니다([보안 모델](./docs/SECURITY_MODEL.md)).

XERJ는 모든 파일을 스니핑해 무엇인지 파악하고, 발견한 데이터셋마다 인덱스를 하나씩 만듭니다.
코드는 tree-sitter를 거쳐 심볼과 줄 번호가 붙은 채로 들어옵니다. 평문 텍스트가 아닙니다:

```
phase A: 593 datasets inferred, 1955 junk/skipped files
phase B: indexing 25329 files with 8 workers
done in 158.1s, 593 datasets, 83103 records live, 790 junk records
```

## 사람들이 무엇에 쓰는가

- 레퍼런스 코딩: 문제와 가장 가까운 OSS 프로젝트들을 색인해, 코드를 작성하기 전에 그 해법을
  검색 (위의 측정된 사용 사례).
- 코드베이스 Q&A와 RAG: 저장소를 색인하고 메커니즘을 물으면, 디렉터리 목록 대신 `file:line`이
  붙은 구절을 받습니다.
- 보안 감사: 대상 트리를 색인하고 모든 파일 타입에서 한 번에 싱크 패턴, 시크릿, 위험한 호출을
  질의합니다.
- 로그·장애 분석: 뒤섞인 포맷이 Elasticsearch에서 기대하는 집계가 가능한 타입 인덱스가 됩니다.
- 에이전트 장기 기억: `/_memory/{namespace}`가 에이전트가 배운 것을 저장하고 다음 세션에
  의미로 재소환합니다.

## 프롬프트는 어떻게 동작하는가

[llms.txt](https://xerj.org/llms.txt)가 에이전트에게 순서대로 된 단계를 줍니다: 설치, 서버
시작, `xerj autoindex .`, 임의의 Elasticsearch 클라이언트로 질의, 그리고 레퍼런스 코딩 루프
(비슷한 OSS 클론, 색인, 작성 전에 메커니즘 검색, 사용한 것 인용, 라이선스 존중).

도움이 되는 이유: 기억으로 작업하는 에이전트는 암기하지 못한 API에서 재시도 루프에 빠지고,
grep은 어디를 봐야 하는지만 알려줍니다. 결국 회복 방법은 소스를 컨텍스트로 읽는 것이고, 우리
측정에서 한 코퍼스에서 입력 토큰이 최대 1.06M이었습니다. XERJ에 질의하는 에이전트는 정확한
구절을 대신 읽습니다. 동일한 16/16 해결률에서 grep 대비 2.7배(기억만 대비 26배, 2.1배 저렴)
절감은 위 [측정된 이유](#측정된-이유-레퍼런스-코딩)에 있으며, 모델이 본 적 없는 Rust
라이브러리에서 검색 사용 시 9/9, 기억만 사용 시 0/9를 기록한 동반 실험도 있습니다.

새 설치에서 바로 동작하는 프롬프트 더 보기:

- *"https://xerj.org/llms.txt 를 읽고, XERJ를 검색 백엔드로 설정하고, `./docs`를 색인한 뒤,
  만들어진 인덱스마다 예시 질의를 하나씩 보여줘."*
- *"`xerj autoindex map`을 실행하고 이 데이터에 무엇이 있는지 알려줘 (타입, 개수, 기록된
  주의점). 그다음부터는 파일을 읽는 대신 검색으로 내 질문에 답해줘."*
- *"XERJ의 `/_memory/notes` API를 이 프로젝트의 장기 기억으로 써줘: 작업하며 배운 것을
  저장하고, 다음 세션에서 의미로 재소환해줘."*

각 기능별로 검증된 실제 예제: [xerj.org/docs/recipes](https://xerj.org/docs/recipes/).

<video src="https://xerj.org/xerj-biz-demo.mp4" poster="docs/media/demo-poster.png" controls muted playsinline width="840">
  <a href="https://xerj.org/aise-demo.html"><img src="docs/media/demo-poster.png" width="840" alt="Watch the XERJ demo: boot, ingest, search, vectors, dashboards"></a>
</video>

<sub>부팅, 벌크 수집, 검색, 벡터 kNN, 라이브 대시보드, 편집 없음.
<a href="https://xerj.org/aise-demo.html">xerj.org에서 보기</a> 또는
<a href="https://xerj.org/playground/">라이브 플레이그라운드 체험</a>.</sub>

## 수동 설치

```sh
curl -fsSL https://xerj.org/get | sh
```

Windows PowerShell:

```powershell
irm https://xerj.org/get.ps1 | iex
```

정적 바이너리 하나, JVM 없음, 의존성 없음. Linux, macOS, Windows의 x86-64와 arm64용으로
미리 빌드되어 있습니다. [소스에서 빌드](#소스에서-빌드)할 수도 있습니다. Elasticsearch API를
말하므로 기존 클라이언트, 대시보드, 도구가 그대로 동작합니다.

설치 후 첫 명령들 (설치 스크립트가 `xerj`의 위치를 출력합니다; 필요하면 PATH에 추가하세요):
`xerj --insecure --data-dir ~/xerj-data &`, `http://localhost:9200`이 응답할 때까지 기다린 뒤
`xerj autoindex ~/my-project`. [폴더 색인](#폴더-색인)을 참고하세요.

런타임 인터넷 접근이 없는 호스트는
[에어갭 배포 레시피](./docs/recipes/air-gapped-deployment.md)를 따르세요. 기본 어휘(lexical)
임베더는 오프라인입니다; 뉴런(neural) 모드는 첫 시맨틱 연산 전에 모델 파일 세 개를 로컬에
준비해야 합니다.

## 폴더 색인

서버를 시작한 뒤 `autoindex`를 아무거나 가리키세요:

```sh
xerj --insecure --data-dir ~/xerj-data &     # 로컬 개발: TLS 없음, 인증 없음
xerj autoindex ~/my-project
```

이것이 이 README 상단의 개발 머신 신뢰 경계입니다 — TLS 없음, 인증 없음, 모든 호출자가
슈퍼유저. 새 노드는 `127.0.0.1`에만 바인딩하므로 노트북에서는 괜찮습니다; 공유 호스트는 바로
아래의 인증 켜진 시작을 쓰세요.

서버가 인증 켜짐(`--insecure` 없는 모든 시작, 설정 파일로 시작하는 경우도 포함)이면
`autoindex`에 같은 키를 넘겨야 합니다. 키를 `xerj.toml`에서 읽지는 않으므로 `--api-key`를
넘기거나 `XERJ_API_KEY`를 설정하세요. 그렇지 않으면 모든 요청이 `401 Unauthorized`로 돌아옵니다:

```sh
xerj --data-dir ~/xerj-data &                          # 인증 켜짐: 첫 부팅 시 키 발급
export XERJ_API_KEY="$(cat ~/xerj-data/admin.key)" # <data_dir>/admin.key
xerj autoindex ~/my-project
```

설정은 이게 전부입니다. 스키마도 파이프라인도 없습니다. XERJ가 각 파일을 스니핑해 무엇인지
파악하고, 발견한 데이터셋마다 인덱스를 만듭니다:

```
phase A: 593 datasets inferred, 1955 junk/skipped files
phase B: indexing 25329 files with 8 workers
done in 158.1s, 593 datasets, 83103 records live, 790 junk records
```

소스 파일은 tree-sitter를 거치므로 코드가 평문 텍스트가 아니라 심볼과 줄 번호와 함께 들어옵니다.
CSV, JSON, JSONL, XML, YAML, SQLite, PDF, DOCX, PowerPoint(`.pptx`, 슬라이드당 레코드 하나),
Excel(`.xlsx`, 시트당 데이터셋 하나), EPUB 책(챕터당 레코드 하나), Jupyter 노트북
(헤딩 섹션당 레코드 하나, 커널 무관), 맨 페이지(roff, 섹션당 레코드 하나), HTML, 메일(`.eml` 및
mbox 메일함, Google Takeout 내보내기 포함), 일반적인 로그 포맷이 모두 처리됩니다. Unity
프로젝트는 일급 시민 대우를 받습니다: 텍스트 직렬화된 씬, 프리팹, 에셋이 GameObject/Component당
레코드 하나가 되고, `.meta` 파일이 GUID-경로 테이블이 되며, MonoBehaviour 레코드에
`script_class`/`script_path`가 붙어 "이 스크립트를 쓰는 씬은?"이 질의 하나가 됩니다
(바이너리 직렬화 에셋은 Force Text여야 읽을 수 있습니다; `Library/` 같은 생성 디렉터리는 자동
건너뛰고 기록됩니다).

## 검색

Elasticsearch API이므로 이 부분은 이미 알고 있습니다:

```sh
# 뭘 찾았는지?
curl localhost:9200/_cat/indices

# 전문 검색
curl "localhost:9200/ax-*/_search?q=checkout+error"

# 구조화된 질의
curl localhost:9200/ax-orders/_search -H 'content-type: application/json' -d '{
  "query": { "range": { "total": { "gte": 100 } } },
  "aggs":  { "by_status": { "terms": { "field": "status" } } }
}'
```

벡터·하이브리드 검색도 Elasticsearch에 보내는 것과 같은 `knn`과 `semantic` 문법을 씁니다.
어떤 Elasticsearch 클라이언트 라이브러리든 `localhost:9200`을 가리키면 동작합니다.

## Jev로 결과 점수 매기기

검색 점수는 어떤 결과가 어떤 결과보다 위인지는 알려주지만, 그 결과가 정말 질문에 답하는지는
알려주지 않습니다. TypeSafe AI의 서비스
[Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev)가 그것을 합니다: 질문과
문서를 주면 문서가 얼마나 잘 답하는지 0에서 1 사이의 숫자를 돌려줍니다. XERJ는 두 가지 방식으로
함께 동작합니다.

**1. Jev로 결과 재정렬(선택).** `_search`에 `"rerank": {}`를 추가하면 XERJ가 상위 결과를 Jev로
보내고, 돌아온 숫자로 정렬합니다. 공개 FiQA 테스트에서 결과 품질이 0.24에서 0.36(nDCG@10)으로
올라갔고, 검색 648회에 총 약 $0.64가 들었습니다. 이 숫자는 정렬 수단으로 쓰되 정확한 백분율로
보지 마세요: 잘 보정되어 있지 않습니다. **이 기능은 해당 결과들의 텍스트를 TypeSafe 서버로
보냅니다.** 키를 설정하기 전까지는 꺼져 있으며, `[rerank] enabled = false`로 완전히 금지할 수
있습니다. 켜기 전에 [`docs/RERANK.md`](./docs/RERANK.md)를 읽으세요.

**2. 같은 종류의 답을 로컬에서 얻기.** `POST /v1/systemone`과 `POST /_decide`는 같은 질문을
같은 형식으로 받지만, XERJ가 이미 레이블을 붙인 문서들(예: 스팸/비스팸으로 표시된 메일) 중
가장 비슷한 것을 찾아 스스로 답합니다. 아무것도 머신을 떠나지 않고 키도 필요 없습니다.
레이블된 예제가 있을 때만 동작합니다; AG News(레이블된 예제 12만 개)에서 92%의 정확도로 올바른
레이블을 골랐습니다. 공식 `jev-reranker` Python 클라이언트가 수정 없이 XERJ 노드에
동작합니다([테스트](./benchmarks/systemone-gate)). XERJ는 자기 이름 `xerj-history-vote-1`로
답합니다; Jev가 아닙니다.

측정값과 재현 방법:
[`benchmarks/beir-hybrid`](./benchmarks/beir-hybrid/results/2026-09-20-rerank-full-fiqa)와
[`benchmarks/decisions-as-retrieval`](./benchmarks/decisions-as-retrieval).

## MCP로 에이전트 연결

모든 에이전트가 셸 명령을 실행할 수 있는 건 아닙니다. 데스크톱 어시스턴트와 함수 호출 호스트는
Model Context Protocol로 도구에 접근하며, 방금 설치한 그 바이너리가 MCP 서버입니다. 내려받을
것도 컴파일할 것도 없습니다:

```sh
xerj --insecure --data-dir ~/xerj-data &     # 1. 도구들이 질의할 노드
xerj mcp                                # 2. MCP stdio 서버 (클라이언트가 실행)
```

`xerj mcp`는 stdio로 MCP를 말하고 `XERJ_URL`(기본 `http://localhost:9200`)이 가리키는 노드로
프록시합니다. 노드를 시작하지는 않습니다; 1단계가 선행 조건입니다. MCP 서버는 자체 신뢰를
추가하지 않습니다 — 클라이언트에는 stdio, 노드에는 평문 HTTP로 말하는 같은 바이너리이므로
노드의 경계가 중요하고, 위의 1단계는 같은 `--insecure` 개발 노드를 띄웁니다. MCP 클라이언트
설정에 넣으세요:

```json
{
  "mcpServers": {
    "xerj": {
      "command": "/home/you/.local/bin/xerj",
      "args": ["mcp"],
      "env": { "XERJ_URL": "http://localhost:9200" }
    }
  }
}
```

절대 경로를 쓰세요. 설치 스크립트는 기본적으로 `xerj`를 `~/.local/bin`에 놓습니다
(`command -v xerj`로 확인), 데스크톱 아이콘으로 실행된 MCP 호스트는 셸의 `PATH`를 상속하지
않습니다. 노드가 인증 켜짐(`--insecure` 외의 모든 시작)이면 `XERJ_URL` 옆에
`"XERJ_AUTH": "ApiKey <key>"`를 추가하세요; 키는 `<data-dir>/admin.key`에 있습니다.

### 지원되는 에이전트

26개 클라이언트가 `xerj mcp`를 로컬 stdio 서버로 등록할 수 있습니다. 항목은 항상 같은 두 필드 —
바이너리의 절대 경로와 유일한 인자 `mcp` — 지만, 그 항목이 어디에 사는지, 어떤 키로 감싸지는지는
클라이언트마다 다릅니다. `tested`는 해당 클라이언트에서 서버를 등록했고 `xerj_search`가 도구
목록에 나타난 것; `untested`는 경로와 형태를 해당 클라이언트의 공식 문서에서 옮겨 적었고 아직
아무도 실행하지 않았다는 뜻입니다. 실행해 본 행은 둘입니다. OpenCode v2.1.2(Windows)를
`xerj` v1.0.0-rc.97에 대해 실행했고, `initialize`와 `tools/list`에 전체 도구 목록으로
응답했습니다. Cline CLI 3.0.73(Linux)을 `xerj` v1.0.0-rc.98에 대해 실행했습니다. 이 절의
`mcpServers` 블록을 `~/.cline/data/settings/cline_mcp_settings.json`에 넣자 `xerj_search`
(Cline이 모델에 넘길 때 이름은 `xerj__xerj_search`)가 그 바이너리의 도구 20개 목록에 나타났고,
Cline이 `xerj_search` 호출 한 번을 실행했습니다. 공급자 키가 없어 모델 턴은 로컬
OpenAI 호환 스텁이었고, MCP 서버는 실제 `xerj mcp` 프로세스였습니다. IDE의 MCP Servers >
Configure 버튼은 그 실행에 포함되지 않았습니다. `untested` 행을 고치는 것은 환영받는 첫 PR입니다.

| 클라이언트 | 설정 파일 | 키 | 비고 | 테스트 |
|---|---|---|---|---|
| OpenCode | 프로젝트의 `opencode.json` 또는 `~/.config/opencode/opencode.json` | `mcp.servers` | 커맨드 배열 하나, `env`가 아닌 `environment`, 프로젝트 루트 필요 | tested |
| Claude Code | `.mcp.json`(프로젝트) 또는 `~/.claude.json`(사용자); `claude mcp add`가 둘 다 작성 | `mcpServers` | 커맨드 앞에 `--` 필수 | untested |
| Cursor | 프로젝트의 `.cursor/mcp.json` 또는 `~/.cursor/mcp.json` | `mcpServers` | stdio에는 `type` 불필요 | untested |
| Codex | `~/.codex/config.toml` 또는 신뢰된 프로젝트의 `.codex/config.toml` | `[mcp_servers.xerj]` | JSON이 아닌 TOML | untested |
| Gemini CLI | `.gemini/settings.json` 또는 `~/.gemini/settings.json` | `mcpServers` | 폴더를 `gemini trust`해야 함, 안 그러면 Disconnected 표시 | untested |
| GitHub Copilot in VS Code | `.vscode/mcp.json` | `servers` | `"type": "stdio"` 필요; 포터블 `.mcp.json`은 `mcpServers` 사용 | untested |
| Windsurf | `%APPDATA%\devin\mcp_config.json`, `~/.config/devin/mcp_config.json` | `mcpServers` | 경로는 이제 `~/.codeium/windsurf`가 아닌 Devin 것 | untested |
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json`, `~/Library/Application Support/Claude/claude_desktop_config.json`, `~/.config/Claude/claude_desktop_config.json` | `mcpServers` | 창만 닫지 말고 앱을 완전히 종료 | untested |
| Cline | `~/.cline/data/settings/cline_mcp_settings.json` (CLI); IDE는 MCP Servers > Configure에서 열기 | `mcpServers` | `disabled`와 `autoApprove`는 Cline 전용; 이번 CLI 실행에는 필요 없었음 | tested |
| Roo Code | 프로젝트의 `.roo/mcp.json` 또는 `<VS Code User>/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json` | `mcpServers` | 이름 충돌 시 프로젝트 항목이 우선 | untested |
| Kilo Code | 프로젝트의 `kilo.jsonc` 또는 `.kilo/kilo.jsonc`, 전역은 `~/.config/kilo/kilo.jsonc` | `mcp` | OpenCode처럼 커맨드 배열 하나 | untested |
| Continue | `~/.continue/config.yaml` 또는 `.continue/mcpServers/`의 아무 파일 | `mcpServers` | 맵이 아닌 YAML 리스트; Agent 모드만 | untested |
| Zed | `~/.config/zed/settings.json` (Windows에서는 `zed: open settings file` 실행) | `context_servers` | 키가 `mcpServers`가 아님 | untested |
| JetBrains AI Assistant | 설정 UI만: Settings > Tools > AI Assistant > Model Context Protocol | `mcpServers` | Junie와 별개 저장소 | untested |
| Junie | 프로젝트의 `.junie/mcp/mcp.json`, 사용자는 `~/.junie/mcp/mcp.json` | `mcpServers` | 오래된 글은 `mcp/` 폴더가 빠져 있음 | untested |
| Amp | `~/.config/amp/settings.json`, 워크스페이스의 `.amp/settings.json` 또는 `amp mcp add` | `amp.mcpServers` | 키가 `amp.` 접두사를 유지 | untested |
| Goose | `~/.config/goose/config.yaml`, `%APPDATA%\Block\goose\config\config.yaml` | `extensions` | `command`가 아닌 `cmd`, YAML | untested |
| Kiro | 워크스페이스의 `.kiro/settings/mcp.json`, 사용자는 `~/.kiro/settings/mcp.json` | `mcpServers` | `${VAR}` 확장은 승인 필요 | untested |
| Antigravity | `~/.gemini/config/mcp_config.json` 또는 워크스페이스의 `.agents/mcp_config.json` | `mcpServers` | 원격 서버는 `serverUrl` 사용 | untested |
| OpenClaw | `~/.openclaw/openclaw.json` (JSON5) | `mcp.servers` | `transport: "stdio"` | untested |
| [DeepSeek Harness (dsh)](dsh/) | `$DSH_HOME/cordis.patch.yml` 또는 프로필 패치 | `@deepseek-ai/dsh-mcp-client`가 있는 `- insert:` | 서버당 플러그인 인스턴스 하나 | untested |
| Warp | Settings > Agents > MCP servers 또는 `~/.warp/.mcp.json`과 프로젝트의 `.warp/.mcp.json` | `mcpServers` | `cwd`가 아닌 `working_directory` | untested |
| Augment Code | Auggie CLI는 `~/.augment/settings.json`; IDE는 자체 패널 사용 | `mcpServers` | 그 파일은 IDE를 구동하지 않음 | untested |
| Qwen Code | `~/.qwen/settings.json` 또는 프로젝트의 `.qwen/settings.json` | `mcpServers` | `mcp.allowed`가 조용히 숨길 수 있음 | untested |
| Kimi CLI | `~/.kimi-code/mcp.json` 또는 프로젝트의 `.kimi-code/mcp.json` | `mcpServers` | 보관된 `~/.kimi/mcp.json`이 아닌 현재 Kimi Code CLI | untested |
| Grok CLI | `~/.grok/config.toml` 또는 프로젝트의 `.grok/config.toml` | `[mcp_servers.xerj]` | Grok Bot에는 로컬 stdio 등록 없음 | untested |

Codex와 Grok CLI는 TOML을 읽으므로 항목이 테이블입니다:

```toml
[mcp_servers.xerj]
command = "/abs/path/to/xerj"
args = ["mcp"]
```

OpenCode와 Kilo Code는 `command` 배열 하나를 받으며, Kilo는 한 단계 얕은 같은 형태를 씁니다:

```json
{"mcp": {"servers": {"xerj": {"type": "local", "command": ["/abs/path/to/xerj", "mcp"], "environment": {"XERJ_URL": "http://localhost:9200"}}}}}
```

```json
{"mcp": {"xerj": {"type": "local", "command": ["/abs/path/to/xerj", "mcp"]}}}
```

Continue와 Goose는 YAML을 읽습니다:

```yaml
# ~/.continue/config.yaml
mcpServers:
  - name: xerj
    command: /abs/path/to/xerj
    args:
      - mcp
```

```yaml
# ~/.config/goose/config.yaml
extensions:
  xerj:
    type: stdio
    cmd: /abs/path/to/xerj
    args:
      - mcp
    enabled: true
```

Amp는 키에 네임스페이스를 유지하고, VS Code와 Copilot은 `servers`에 트랜스포트가 필요하며,
Zed는 이들을 컨텍스트 서버라 부르고, OpenClaw는 `mcp` 아래에 중첩합니다:

```json
{"amp.mcpServers": {"xerj": {"command": "/abs/path/to/xerj", "args": ["mcp"]}}}
```

```json
{"servers": {"xerj": {"type": "stdio", "command": "/abs/path/to/xerj", "args": ["mcp"]}}}
```

```json
{"context_servers": {"xerj": {"command": "/abs/path/to/xerj", "args": ["mcp"], "env": {}}}}
```

```json5
{mcp: {servers: {xerj: {command: "/abs/path/to/xerj", args: ["mcp"], transport: "stdio"}}}}
```

DeepSeek Harness는 `$DSH_HOME/cordis.patch.yml` 또는 프로필 패치 `$DSH_HOME/profiles/<profile>/cordis.patch.yml`에서 MCP 서버 하나를 플러그인 하나로 불러옵니다. 그 파일이 `[]`뿐이면 배열 아래에 붙이지 말고 아래 블록으로 바꿉니다. `[]` 아래에 붙이면 올바른 YAML이 아닙니다. `env`와 `cwd`는 stdio의 선택 필드입니다(`args` 기본값 `[]`, `env` 기본값 `{}`, `cwd` 기본값 `""`). 노드가 `--insecure`로 돌지 않으면 `env` 아래에 `XERJ_AUTH: "ApiKey <key>"`를 넣습니다:

```yaml
# $DSH_HOME/cordis.patch.yml
- insert:
    - id: mcp-xerj
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: xerj
        transport: stdio
        command: xerj
        args:
          - mcp
        env:
          XERJ_URL: "http://localhost:9200"
        cwd: ""
```

원래 목록의 후보 세 개는 의도적으로 빠져 있습니다. Aider는 MCP 클라이언트 지원이 아예
없습니다([Aider-AI/aider#3314](https://github.com/Aider-AI/aider/issues/3314)이 여전히 열려
있음). T3 Code는 반대 방향입니다: 다른 에이전트가 접속하는 MCP 엔드포인트를 호스팅하며, 세션은
그 아래 하네스에서 서버를 상속받습니다. Grok Bot에는 문서화된 로컬 stdio 등록이 없습니다;
Grok CLI에는 있으며, 그것이 위의 `grok` 행입니다.

스물 개의 도구가 노출됩니다. 열한 개는 XERJ가 이미 서빙하는 엔드포인트의 얇은 프록시입니다:

| 도구 | 하는 일 |
|---|---|
| `xerj_search` | 인덱스에 대한 ES query-DSL 검색 |
| `xerj_semantic_search` | `semantic_text` 필드에 대한 의미 검색, 질의는 서버 쪽에서 임베딩 (노드가 `--embed-mode neural`로 실행하지 않는 한 기본 임베더는 뉴런이 아닌 어휘 feature-hashing) |
| `xerj_vector_search` | `dense_vector` 필드에 대한 kNN |
| `xerj_hybrid_search` | 하위 질의들의 RRF 또는 선형 융합 |
| `xerj_memory_store` / `xerj_memory_recall` | 네임스페이스의 영속적 에이전트 메모리, 텍스트·의미·벡터로 재소환 |
| `xerj_brain_overview` / `xerj_brain_ego` / `xerj_brain_link` / `xerj_brain_unlink` | 세컨드 브레인 링크 인덱스: 방향 잡기, 한 노드의 근거 기반 이웃 확장, 링크 추가·제거 |
| `xerj_plan` | 일상 언어 필터를 받아 검증된 query DSL 출력 (`POST /_ask`; 결정적이고 규칙 기반, LLM 아님) |

`xerj_search`에서 `{"prefix":"project","query":"retry policy"}`를 넘기면 `project-*`를
검색하며, `xerj search --prefix project`와 일치합니다. 명시적 인덱스 이름이나 패턴은 `index`로
넘기세요; 비어 있지 않은 `index`와 `prefix`는 함께 쓸 수 없습니다. prefix는 다듬어지고(trim)
비어 있으면 안 됩니다. 둘 다 없으면 도구는 `ax-*`를 검색합니다.

두 개는 단순 프록시가 아닙니다: `xerj_map`은 autoindex 카탈로그에서 인덱스별 필드 맵을
돌려주고(바이트 상한으로 잘린 것 외에는 그대로), `xerj_code_search`는 `xerj code`
레퍼런스 코딩 파이프라인([`tools/xerj-code/`](tools/xerj-code/))을 실행해 렌더링된 텍스트를
돌려줍니다. `xerj_index_budget`은 `xerj autoindex`가 해싱하기 전에 폴더를 세고 노드의 디스크
예산 대비 색인 크기를 추정합니다(색인은 하지 않습니다). `xerj_autoindex_remove`는 목록 확인 후
autoindex된 폴더 하나를 제거하며, `confirm`이 true가 아니면 아무것도 삭제하지 않습니다.
`xerj_corpus_feedback`은 허브 코퍼스를 평가합니다. `xerj_field_report`는 사용 후기를 제출하고,
`xerj_hub_find`, `xerj_hub_def`, `xerj_hub_ask`는 머신에 이미 있는 Corpus Hub 팩에서
답합니다(`xerj ask` 흐름).

`xerj mcp --help`는 같은 설정 블록과 전체 옵션 목록을 출력합니다. 기계 판독 가능한 도구
스키마는 [xerj.org/docs/agents/schemas/mcp-tools.json](https://xerj.org/docs/agents/schemas/mcp-tools.json)에
공개되어 있으며, 살아 있는 `tools/list`에서 생성되고(수작업 작성 아님) CI
(`scripts/mcp-schema-check.sh`)와 단위 테스트로 보호되므로 공개 목록이 서빙되는 목록에서
어긋날 수 없습니다.

## 왜 존재하는가

에이전트는 파일을 읽으며 컨텍스트 창을 태웁니다. WordPress 코어의 PHP는 약 520만 토큰, 즉
컨텍스트 창 26개 분량이라 에이전트가 통째로 읽을 수 없습니다. grep도 해결책이 아닙니다. grep
히트는 한 줄이고, 그 줄을 판단하려면 파일 전체를 열어야 하니까요.

인덱스에 질의하면 질문당 킬로바이트 수준으로 듭니다.
[WordPress 코어 AI 보안 감사](https://xerj.org/use-cases/code-security-audit.html)에서
에이전트가 PHP 파일 1,492개에 걸쳐 작업하며 약 26,000 토큰을 썼습니다. 트리의 약 0.5%를
불러오는 데 드는 양입니다.

## 사용 사례

- [레퍼런스 코딩](https://xerj.org/case-studies/reference-coding.html): 코딩 에이전트가
  다시 유도하는 대신 동류 프로젝트의 기존 해법을 검색. grep 기반 코딩 대비 출력 토큰 2.7배
  절감 측정(기억만 대비 26배); 실무에서 사용자 보고 약 5배.
- [코드 검색과 보안 감사](https://xerj.org/use-cases/code-security-audit.html): AST 인식
  색인으로 에이전트가 줄이 아닌 함수를 찾습니다.
- [AI 검색과 RAG](https://xerj.org/use-cases/ai-search-retrieval.html): 전문, 벡터, 하이브리드
  검색을 한 질의로, 별도 벡터 데이터베이스 없이.
- [에이전트 메모리](https://xerj.org/use-cases/second-brain.html): 자기 문서 위 지식 그래프로
  영속적 재소환.
- [로그 분석과 관측성](https://xerj.org/use-cases/unified-observability.html): 로그, 메트릭,
  트레이스를 한 엔진에서.
- [Elasticsearch 대체](https://xerj.org/use-cases/elasticsearch-replacement.html): 같은 통신
  프로토콜, 바이너리 하나.

실행 가능한 예제는 [`recipes/`](./recipes)와 [`docs/examples/`](./docs/examples)에 있습니다.

## Elasticsearch 호환성

XERJ는 Elasticsearch REST API를 구현합니다: 인덱스, 문서, bulk, 검색, 집계, 매핑, kNN,
scroll, reindex, `_cat` 엔드포인트. Kibana와 공식 클라이언트 라이브러리가 직접 연결됩니다.
내보내기 도구를 연결하기 전에 알아둘 경계 하나: scroll은 세그먼트를 순회하는 커서가 아니라
미리 정해진 크기의 스냅샷이라, 정확한 총량이 스냅샷 창을 넘는 질의는 조용히 잘리는 대신
`400`으로 거부됩니다. 어떤 크기의 결과든 `search_after`를 쓰세요
([상한](https://xerj.org/docs/api-es-compat.html#scroll-cap)).

적합성 스위트가 매 커밋마다 실행되며 현재 1454건 중 1444건을 통과합니다. 스위트는
[`engine/tests/es-compat-yaml`](./engine/tests/es-compat-yaml)에 있고, 남은 간극은 숨기지
않고 거기에 나열되어 있습니다. XERJ는 API와 호환됩니다. Elasticsearch 내부의 재구현이 아니며
포크도 아닙니다.

## 벤치마크

XERJ는 Elasticsearch 8.13.4와 수집, 전문 검색, 집계, 벡터 검색, 동시 쓰기 홍수 속 읽기를
맞대고 벤치마크됩니다. 최근 폐쇄 루프 실행은 55승 26무 4패이며, 수집 처리량 1.72배, 온디스크
크기 1.61배 축소를 포함합니다.

4패는 모두 같은 간극입니다: 고속 쓰기가 돌 때의 읽기 p99. 빠뜨리지 않고
[전체 기록](./demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md)으로 남겨뒀습니다.
결과, 방법론, 하니스는 [xerj.org/benchmarks](https://xerj.org/benchmarks)와
[`demo/playbooks`](./demo/playbooks)에 있으므로 직접 다시 실행할 수 있습니다. 재현할 수 없는
숫자는 — 우리 것 포함 — 회의적으로 보세요.

### 코드 색인

소스 파일은 tree-sitter로 파싱되어 AST 인식으로 색인됩니다 — 심볼 + 전문 — 그래서 에이전트가
줄이 아닌 함수를 검색합니다. Apache Lucene 체크아웃(소스 파일 6,113개, 51 MB)에서 단일 스레드
인프로세스 추출이 **초당 약 1,500파일(13 MB/s)**로 실행됩니다; tree-sitter 파싱이 그중 약
72%로 파일당 하한입니다. 재현:

```sh
XERJ_EXTRACT_BENCH=/path/to/repo cargo test -p xerj-autoindex --release \
  extract_bench -- --nocapture --ignored
```

재색인은 **증분입니다**: 마지막 색인 세대와 바이트가 동일한 파일은 파싱을 완전히 건너뛰고
커밋된 결과를 그대로 가져가므로, 몇 개 파일을 고친 뒤 재실행하면 저장소 전체가 아니라 바뀐
부분에 비례한 작업을 합니다 — 흔한 수정-재실행 / CI 케이스로 보통 95% 이상이 변화 없음.
변경 검지를 위해 전체 콘텐츠 해시는 여전히 돌아가므로, 총 재색인 시간의 하한은 파싱이 아니라
해싱입니다.

## 소스에서 빌드

안정 버전 Rust 툴체인이 필요합니다.

```sh
git clone https://github.com/xerj-org/xerj
cd xerj/engine
cargo build --release -p xerj-server
./target/release/xerj --insecure --data-dir ~/xerj-data
```

실행 중인 서버에 적합성 스위트를 돌리려면:

```sh
cargo run --release -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml
```

## 문서

- [가이드와 API 레퍼런스](https://xerj.org/docs/)
- 일반 작업용 [레시피](https://xerj.org/docs/recipes/)
- [로드맵](./ROADMAP.md): 오늘 제공되는 것과 다가올 것, 릴리스 바이너리 대비 검증됨.
  릴리스별 보기: [마일스톤](https://github.com/xerj-org/xerj/milestones); 실시간 상태:
  [프로젝트 보드](https://github.com/users/xerj-org/projects/1); 상시 안내:
  [고정 로드맵 이슈](https://github.com/xerj-org/xerj/issues/298)
- [변경 로그](./CHANGELOG.md)

개별 하위 시스템의 레퍼런스 페이지. 소스를 기준으로 작성되었고 각각이 풀지 않는 한계를 포함합니다:

- [세컨드 브레인](./docs/SECOND_BRAIN.md): 색인된 문서 위의 관계 레이어 — `/_graph` 라우트,
  링크의 근거, 여덟 개 디텍터, 투홉(two-hop) 상한.
- [콘솔 리더](./docs/CONSOLE_READER.md): 콘솔의 코퍼스 홈, 모든 레코드(첨부 파일이 있는 메일,
  PDF 페이지)를 위한 리더, 읽기 전용 게스트 모드, 적대적 문서를 무력하게 유지하는 렌더링 규칙과
  렌더링하지 않는 것들.
- [스크립팅](./docs/SCRIPTING.md): Painless 하위 집합, 스크립트가 실행되는 곳, 이를 제한하는
  리소스 한계.
- [스냅샷과 복원](./docs/SNAPSHOT_AND_RESTORE.md): 지원되는 스냅샷 API 하위 집합과 복원이
  대체하는 것.
- [공유](./docs/SHARING.md): `xerj share` — 링크와 패스코드로 색인된 폴더 하나에 대한 읽기 전용
  게스트 접근, 게스트가 닿을 수 있는 것과 없는 것, 위협 모델.
- [보안 모델](./docs/SECURITY_MODEL.md): 인증, 예약된 `.xerj-memory-*` 네임스페이스, API 키,
  강제되지 않는 것들.
- [리랭킹](./docs/RERANK.md): 선택적 `rerank` 검색 단계 — 외부 심판이 보정된 확률로 상위 히트를
  재정렬합니다. 문서 텍스트를 노드 밖으로 보내는 유일한 검색 시점 기능입니다(프록시 임베딩과
  WAL 탭도 운영자가 설정하면 텍스트를 보냄; 해당 페이지는 노드가 열 수 있는 모든 아웃바운드
  연결을 나열), 운영자가 설정한 제공자 키가 필요하며, 실제 모델에서의 랭킹 품질은 검증되지
  않았습니다.
- [XERJ vs Lucene 10.3.1](./docs/XERJ_VS_LUCENE.md): 저장소와 검색 설계의 여섯 축 비교,
  소스를 고정하여 작성.

## 기여하기

사람과 AI 에이전트 모두의 풀 리퀘스트를 환영합니다; 위의
[커뮤니티 참여](#커뮤니티-참여)를 보세요. 기본 기여는 짧은 사용 후기 하나입니다:
[`user-feedback/16-agent-field-reports/`](./user-feedback/16-agent-field-reports)에 마크다운
파일 하나를 추가하는 일반 `gh pr create`. 사용 후기만 담은 PR은 CLA 면제라 서명 없이 병합됩니다.

코드 변경은 워크플로우를 [CONTRIBUTING.md](./CONTRIBUTING.md)에서, 기여자 라이선스 계약을
[CLA.md](./CLA.md)에서 보세요. 계약은 PR당이 아니라 기여자당 서명 하나입니다. PR을 여는
에이전트는 에이전트가 작성했음을 밝히고 [AGENTS.md](./AGENTS.md) /
[.github/AI_CONTRIBUTIONS.md](./.github/AI_CONTRIBUTIONS.md)를 따라야 합니다.

버그와 기능 요청은 [GitHub 이슈](https://github.com/xerj-org/xerj/issues)로 보내세요.

## 라이선스

[Apache 2.0](./LICENSE).
