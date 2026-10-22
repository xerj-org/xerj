# XERJ

**Yapay zekâ ajanları için yerel öncelikli arama motoru ve MCP sunucusu.** Tek bir Rust ikilisi; kodu, belgeleri, günlükleri, JSONL/CSV dosyalarını ve PDF'leri indeksleyerek BM25 tam metin, vektör ve hibrit arama, kod arama ve ajan belleği sunar. Elasticsearch API'siyle uyumludur; Claude Code, Cursor ve tüm MCP istemcileriyle çalışır.

<a href="https://trendshift.io/repositories/116361?utm_source=repository-badge&amp;utm_medium=badge&amp;utm_campaign=badge-repository-116361" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/repositories/116361" alt="xerj-org%2Fxerj | Trendshift" width="250" height="55"/></a>

[English](./README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja-JP.md) | [العربية](./README.ar.md) | [Español](./README.es.md) | [Türkçe](./README.tr.md)

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
[![ES-YAML conformance](https://img.shields.io/badge/ES--YAML%20conformance-1366%2F1369-brightgreen?style=flat-square)](https://xerj.org/benchmarks)
[![Single static binary](https://img.shields.io/badge/single%20static%20binary-no%20JVM-orange?style=flat-square)](https://xerj.org/docs/install.html)
[![Default embedder](https://img.shields.io/badge/default%20embedder-lexical%2C%20offline-teal?style=flat-square)](https://xerj.org/docs/vectors.html)
[![Neural](https://img.shields.io/badge/neural%20embedder-opt--in%2C%20downloads%20~90%20MB-9c6ade?style=flat-square)](https://xerj.org/docs/vectors.html)

Bu sayfa [İngilizce README'nin](./README.md) Türkçe çevirisidir. Farklılık olması hâlinde İngilizce sürüm esas alınmalıdır. Ölçüm, maliyet ve örnek çıktıları kaynak metinden aktarılmıştır; bu çeviride yeniden ölçülmemiştir.

XERJ, herhangi bir klasörü otomatik olarak indeksleyen yerel bir yapay zekâ aramasıdır. Kodlama ajanınızın dosyaları tek tek okuyarak bağlam penceresini tüketmesi yerine ihtiyaç duyduğu kod parçalarını bulmasını sağlar. En belirgin kullanım alanı referansla kodlamadır: ajana bir görev verirsiniz; ajan, benzer açık kaynak projelerini indirip indeksler ve kod yazmadan önce bu projelerin sorunu nasıl çözdüğünü arar. Kontrollü bir çalışmada, çözüm oranı 16/16 kalırken ajanın çıktı tokenlarını 2,7 kat azalttı ([vaka çalışması](https://xerj.org/case-studies/reference-coding.html)). Kullanıcılar günlük çalışmada yaklaşık 5 kat daha az token bildirmiştir ([saha raporları](./user-feedback/11-reference-coding-field-reports/2026-08-11-token-savings-reports.md)). Sonuç, kullanılan kodun modelin belleğinde olup olmamasına bağlıdır: özel, kurum içi, az bilinen veya eğitim kesim tarihinden sonraki kodlarda fayda sağlar; modelin zaten iyi bildiği popüler açık kaynak kütüphanelerde etkisiz kalabilir, hatta ek yük oluşturabilir. Ayrıntılar ve sınırlar [vaka çalışmasında](https://xerj.org/case-studies/reference-coding.html) açıklanır.

Tek bir istemle kurulum için ajana şu yönergeyi verin:

```text
XERJ'yi kur (belgeler: https://xerj.org/llms.txt), bu projenin kaynaklarını indeksle ve referansla kodlama akışını kur: üzerinde çalıştığımız konuya en yakın açık kaynak depolarını klonlayıp indeksle, kod yazmadan önce sorunu nasıl çözdüklerini ara.
```

Ya da kendiniz çalıştırın:

```sh
curl -fsSL https://xerj.org/get | sh
xerj --insecure --data-dir ~/xerj-data &
xerj init
xerj autoindex ~/my-project
xerj search "how do we rotate the WAL segment"
xerj def "euler_to_rotationmatrix"
xerj gain
```

Veri dizinini indeksleyeceğiniz klasörlerin dışında tutun. `data/`, yerleşik yok sayma listesinde değildir; indekslenen klasörün içine `--data-dir ./data` koyarsanız düğüm kendi verisini de indeksler. `--insecure` TLS'yi ve API anahtarı doğrulamasını kapatır; porta erişebilen herkes tüm indeksleri okuyup silebilir. Bu seçenek tek kullanıcılı geliştirme makinesi içindir. Yeni düğüm varsayılan olarak yalnızca `127.0.0.1` adresine bağlanır; açık metin bağlantısını başka bir arayüzde başlatmak için açıkça izin vermeniz gerekir. Ayrıntılar için [güvenlik modeline](./docs/SECURITY_MODEL.md) bakın. Windows ve elle doğrulanmış kurulum [kurulum bölümünde](#elle-kurulum) anlatılır.

## Topluluğa katılın

XERJ'yi çalıştırıp deneyiminizi anlatan kısa bir saha raporu göndererek topluluğa katılabilirsiniz: tek bir commit veya PR yeterlidir. Apache-2.0 lisanslı ikiliyi ücretsiz sunan proje, karşılığında yalnızca bu raporu ister. Böylece sonraki kullanıcılar için motor geliştirilebilir.

İnsanlar ve yapay zekâ ajanları katkı sunabilir. Bir depoyu indekslemiş olmanız ya da bir ajanın başka bir kod tabanında XERJ'yi kurması fark etmez: neyi indekslediğinizi, ne için kullandığınızı ve dürüst değerlendirmenizi anlatan kısa bir rapor gönderin. Olumsuz geri bildirim de değerlidir. Bir şey bozulduysa bunun için ayrıca sorun veya düzeltme açın.

Yalnızca saha raporu içeren PR'lar CLA imzası gerektirmez. Her sürümde kullanılabilecek yöntem, [`user-feedback/16-agent-field-reports/`](./user-feedback/16-agent-field-reports/) altına tek bir Markdown dosyası eklemektir. `xerj feedback` komutu rapor taslağı hazırlar; `xerj feedback --open-pr` PR açar. Komutun sürümünüzde bulunup bulunmadığını `xerj --help` ile kontrol edin.

## Referansla kodlama neden işe yarıyor?

Grep kullanan bir ajan, ölçümlerde tek bir derlemden 1,06 milyon giriş tokenına kadar kaynak kodu bağlamına alıp yine de dosyaları okumak zorunda kaldı. XERJ'ye sorgu gönderen ajan doğrudan ilgili parçayı alır. Modelin ezberlemediği kod üzerinde yapılan uçtan uca ölçüm ([vaka çalışması](https://xerj.org/case-studies/reference-coding.html); 4 dilde 8 görev, her grup için 16 çalışma ve gerçek `claude -p` token sayımları):

| Yöntem | Çıktı tokenı | Maliyet | Çözülen görev |
|---|---:|---:|---:|
| Yalnızca model belleği | 260.916 | $11.18 | 11/16 |
| Grep kullanan ajan | 26.477 | $3.27 | 16/16 |
| XERJ | 9.982 | $1.58 | 16/16 |

Aynı çözüm oranında XERJ, grep'e kıyasla 2,7 kat, yalnızca belleğe kıyasla 26 kat daha az çıktı tokenı kullandı; tek bir Java görevinde fark 278 kata ulaştı. Gerçek geliştirmede kullanıcılar uçtan uca yaklaşık 5 kat az token bildirdi ([saha raporları](./user-feedback/11-reference-coding-field-reports/2026-08-11-token-savings-reports.md)). Bu avantaj modelin koda aşinalığına bağlıdır; popüler ve modelin zaten bildiği kütüphanelerde aynı fayda beklenmemelidir. Sınırlar [vaka çalışmasında](https://xerj.org/case-studies/reference-coding.html) yer alır.

## Herhangi bir klasörü besleyin

`xerj autoindex <folder>` bir derlemi grep etmek yerine ajanın sorgulayabileceği hâle getirir. Tek komutla kod, belgeler, günlükler, PDF'ler, SQLite ve karmaşık CSV dosyaları arama, RAG, güvenlik denetimi ve ajan belleği için türlendirilmiş indekslere dönüşür. Şema veya veri hattı yazmanız gerekmez.

```sh
xerj --insecure --data-dir ~/xerj-data &
xerj autoindex ~/my-project
```

XERJ her dosyanın türünü saptar ve bulduğu her veri kümesi için bir indeks oluşturur. Kodlar düz metin yerine tree-sitter üzerinden sembolleri ve satır numaralarıyla indekslenir. CSV, JSON, JSONL, XML, YAML, SQLite, PDF, DOCX, PowerPoint, Excel, EPUB, Jupyter not defterleri, man sayfaları, HTML, e-posta dosyaları ve yaygın günlük biçimleri desteklenir. PowerPoint'te her slayt, Excel'de her sayfa, EPUB'da her bölüm ayrı kayıt olur. Unity projelerinde metin biçimindeki sahne, prefab ve varlıklar GameObject/Component başına kayda dönüştürülür; `.meta` dosyaları GUID-yol tablosu olur. İkili serileştirilmiş Unity varlıklarının okunması için Force Text gerekir; `Library/` gibi üretilen klasörler otomatik atlanır.

## Kullanım alanları

- **Referansla kodlama:** Soruna yakın açık kaynak projelerini indeksleyip kod yazmadan önce çözümlerini bulun.
- **Kod tabanı soru-cevap ve RAG:** Depoyu indeksleyin; klasör listesi yerine `dosya:satır` bilgisiyle ilgili parçayı alın.
- **Güvenlik denetimi:** Dosya türleri arasında tehlikeli çağrıları, gizli anahtarları ve hedef kalıplarını arayın.
- **Günlük ve olay analizi:** Farklı biçimleri Elasticsearch'ten beklenen toplulaştırmalarla türlendirilmiş indekslere dönüştürün.
- **Ajan belleği:** `/_memory/{namespace}` ile ajanın öğrendiklerini saklayıp sonraki oturumda hatırlayın.

## İstem nasıl çalışır?

[llms.txt](https://xerj.org/llms.txt), ajana kurulum, sunucuyu başlatma, `xerj autoindex .` çalıştırma, Elasticsearch istemcisiyle sorgulama ve referansla kodlama adımlarını sırasıyla verir. Örnekler ve her özellik için doğrulanmış tarifler [xerj.org/docs/recipes](https://xerj.org/docs/recipes/) adresindedir.

```sh
xerj --insecure --data-dir ~/xerj-data &
xerj autoindex .
xerj search "how do we rotate the WAL segment"
```

Yeni kurulumda işe yarayan örnek istemler:

- “https://xerj.org/llms.txt dosyasını oku, XERJ'yi arama arka ucum olarak kur, `./docs` klasörünü indeksle ve oluşturduğu her indeks için bir örnek sorgu göster.”
- “`xerj autoindex map` çalıştırıp bu veride neler olduğunu (türler, sayılar ve kaydedilen uyarılar) anlat; sonra dosyaları okumak yerine sorularımı aramayla yanıtla.”
- “Bu projede `/_memory/notes` API'sini uzun süreli belleğin olarak kullan: çalışırken öğrendiklerini kaydet ve sonraki oturumda anlamına göre hatırla.”

## Elle kurulum

```sh
curl -fsSL https://xerj.org/get | sh
```

Windows PowerShell:

```powershell
irm https://xerj.org/get.ps1 | iex
```

Tek bir statik ikili dosyadır; JVM veya bağımlılık gerektirmez. Linux, macOS ve Windows için x86-64 ve arm64 derlemeleri bulunur. İsterseniz [kaynaktan derleyebilirsiniz](#kaynaktan-derleme). Elasticsearch API'siyle konuştuğu için mevcut istemciler, panolar ve araçlar değişiklik yapılmadan kullanılabilir.

Kurulumdan sonra `xerj --insecure --data-dir ~/xerj-data &` ile sunucuyu başlatın, `http://localhost:9200` yanıt verene kadar bekleyin ve `xerj autoindex ~/my-project` çalıştırın. İnternet erişimi olmayan sunucular için [ağ bağlantısız dağıtım tarifine](./docs/recipes/air-gapped-deployment.md) bakın. Varsayılan sözcüksel gömücü çevrimdışı çalışır; sinir ağı modu için ilk anlamsal işlemden önce üç model dosyası yerel olarak hazırlanmalıdır.

## Bir klasörü indeksleme

Sunucuyu başlatıp `autoindex` komutunu klasöre yönlendirin:

```sh
xerj --insecure --data-dir ~/xerj-data &
xerj autoindex ~/my-project
```

`--insecure` TLS ve kimlik doğrulamayı kapatır; porta erişen her istemci yönetici yetkisine sahip olur. Yeni düğüm yalnızca `127.0.0.1` adresinde dinlediği için dizüstü bilgisayardaki yerel geliştirme kullanımına uygundur. Paylaşılan sunucuda kimlik doğrulaması açık başlatın. `--insecure` kullanılmadan başlatıldığında (yapılandırma dosyasıyla başlatma dâhil) kimlik doğrulaması varsayılan olarak açıktır. Bu durumda `autoindex` komutuna aynı anahtarı `--api-key` ile veya `XERJ_API_KEY` ortam değişkeniyle verin; anahtar `<data_dir>/admin.key` dosyasındadır. Anahtar `xerj.toml` dosyasından otomatik alınmaz.

Kaynak dosyaları tree-sitter ile işlenir; kod düz metin yerine semboller ve satır numaralarıyla gelir. CSV, JSON, JSONL, XML, YAML, SQLite, PDF, DOCX, PowerPoint (`.pptx`, slayt başına kayıt), Excel (`.xlsx`, sayfa başına veri kümesi), EPUB (bölüm başına kayıt), Jupyter not defterleri, man sayfaları, HTML, `.eml` ve mbox e-postaları (Google Takeout dışa aktarımı dâhil) ve yaygın günlük biçimleri desteklenir. Unity projelerinde metin biçiminde serileştirilmiş sahne, prefab ve varlıklar GameObject/Component başına kayda dönüşür. `.meta` dosyalarından GUID-yol tablosu oluşturulur; MonoBehaviour kayıtları `script_class` ve `script_path` taşır. İkili serileştirilmiş varlıklar için Force Text gerekir; `Library/` gibi üretilen klasörler otomatik atlanıp raporlanır.

## Arama

Elasticsearch API'si kullanılır:

```sh
# İndeksleri listele
curl localhost:9200/_cat/indices

# Tam metin araması
curl "localhost:9200/ax-*/_search?q=checkout+error"

# Yapılandırılmış arama ve toplulaştırma
curl localhost:9200/ax-orders/_search -H 'content-type: application/json' -d '{
  "query": { "range": { "total": { "gte": 100 } } },
  "aggs":  { "by_status": { "terms": { "field": "status" } } }
}'
```

Vektör ve hibrit arama, Elasticsearch'e gönderdiğiniz `knn` ve `semantic` sözdizimini kullanır. Herhangi bir Elasticsearch istemci kütüphanesini `localhost:9200` adresine yönlendirebilirsiniz.

## Jev ile sonuçları puanlama

Arama puanı sonuçların sırasını verir; ancak bir sonucun soruyu gerçekten yanıtlayıp yanıtlamadığını göstermez. TypeSafe AI hizmeti [Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev), bir soru ve belge alıp belgenin soruyu ne ölçüde yanıtladığını 0 ile 1 arasında puanlar. XERJ, Jev'yi iki biçimde kullanır.

**1. Sonuçları Jev ile yeniden sıralama (isteğe bağlı).** `_search` isteğine `"rerank": {}` ekleyin. XERJ üst sonuçları Jev'ye gönderir ve dönen puanlara göre sıralar. Açık FiQA testinde sonuç kalitesi (nDCG@10) 0,24'ten 0,36'ya çıktı; 648 arama toplam yaklaşık 0,64 dolara mal oldu. Puanları kesin yüzdeler değil, sıralama aracı olarak değerlendirin; iyi kalibre edilmemiştir. Bu özellik sonuç metnini TypeSafe sunucularına gönderir. Anahtar ayarlanana kadar kapalıdır; `[rerank] enabled = false` ile tamamen devre dışı bırakılır. Açmadan önce [`docs/RERANK.md`](./docs/RERANK.md) belgesini okuyun.

**2. Benzer yanıtı yerel olarak alma.** `POST /v1/systemone` ve `POST /_decide` aynı biçimde soru kabul eder; XERJ, önceden etiketlediğiniz en benzer belgeleri (örneğin spam/spam değil olarak işaretlenmiş iletileri) bulup yanıtı kendi üretir. Veri makinenizden çıkmaz ve anahtar gerekmez. Etiketli örnekleriniz olmalıdır; 120 bin etiketli örnek içeren AG News'te doğru etiketi %92 oranında seçer. Resmî `jev-reranker` Python istemcisi değişiklik yapılmadan XERJ düğümüyle çalışır ([test](./benchmarks/systemone-gate)). XERJ yanıtı `xerj-history-vote-1` adıyla verir; bu Jev değildir.

Ölçümler ve yeniden üretme adımları: [`benchmarks/beir-hybrid`](./benchmarks/beir-hybrid/results/2026-09-20-rerank-full-fiqa) ve [`benchmarks/decisions-as-retrieval`](./benchmarks/decisions-as-retrieval).

## MCP ile ajan bağlama

Her ajan kabuk komutu çalıştıramaz. Masaüstü yardımcıları ve araç çağıran sistemler araçlara Model Context Protocol (MCP) üzerinden ulaşır. Kurduğunuz ikili aynı zamanda MCP sunucusudur; ayrıca bir şey indirmeniz veya derlemeniz gerekmez:

```sh
xerj --insecure --data-dir ~/xerj-data &
xerj mcp
```

`xerj mcp`, standart girdi/çıktı üzerinden MCP konuşur ve `XERJ_URL` ile belirtilen düğüme (varsayılan `http://localhost:9200`) istekleri iletir. Düğümü başlatmaz; ilk komut ön koşuldur. MCP sunucusu kendine ait bir güvenlik sınırı eklemez: düğümün sınırı geçerlidir ve yukarıdaki örnek kimlik doğrulamasız geliştirme düğümünü başlatır. İstemcinizin yapılandırmasına şunu ekleyin:

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

Mutlak yol kullanın. Kurucu varsayılan olarak `xerj` dosyasını `~/.local/bin` altına koyar (`command -v xerj` ile bulunur); masaüstü simgesinden açılan MCP istemcileri kabuğunuzun `PATH` değerini devralmaz. Düğümde kimlik doğrulaması açıksa `XERJ_URL` yanına `"XERJ_AUTH": "ApiKey <key>"` ekleyin; anahtar `<data-dir>/admin.key` dosyasındadır.

### Desteklenen ajanlar

Yirmi altı istemci `xerj mcp` komutunu yerel stdio sunucusu olarak kaydedebilir. Girişte ikilinin mutlak yolu ve tek argüman olarak `mcp` bulunur; dosya konumu ve anahtar adı istemciye göre değişir. `tested`, sunucunun istemciye kaydedildiği ve `xerj_search` aracının listede göründüğü anlamına gelir. `untested`, yol ve biçimin istemcinin belgelerinden aktarıldığı ve henüz çalıştırılmadığı anlamına gelir. Yalnızca OpenCode satırı Windows'ta OpenCode v2.1.2 ve `xerj` v1.0.0-rc.97 ile çalıştırılmıştır; `initialize` ve `tools/list` tam araç listesini döndürmüştür. Henüz denenmemiş bir satırı doğrulamak iyi bir ilk PR olabilir.

| İstemci | Yapılandırma dosyası | Anahtar | Not | Durum |
|---|---|---|---|---|
| OpenCode | Projede `opencode.json` veya `~/.config/opencode/opencode.json` | `mcp.servers` | Tek komut dizisi; `env` yerine `environment`; proje kökü gerekir | tested |
| Claude Code | Projede `.mcp.json` veya kullanıcıda `~/.claude.json`; `claude mcp add` ikisini de yazar | `mcpServers` | Komuttan önce `--` gerekir | untested |
| Cursor | Projede `.cursor/mcp.json` veya `~/.cursor/mcp.json` | `mcpServers` | stdio için `type` eklenmez | untested |
| Codex | `~/.codex/config.toml` veya güvenilen projelerde `.codex/config.toml` | `[mcp_servers.xerj]` | JSON değil TOML | untested |
| Gemini CLI | `.gemini/settings.json` veya `~/.gemini/settings.json` | `mcpServers` | Klasörü `gemini trust` ile güvenilir yapın | untested |
| GitHub Copilot (VS Code) | `.vscode/mcp.json` | `servers` | `"type": "stdio"` gerekir | untested |
| Windsurf | `%APPDATA%\devin\mcp_config.json` veya `~/.config/devin/mcp_config.json` | `mcpServers` | Yol artık Devin'e aittir | untested |
| Claude Desktop | Windows, macOS veya Linux yapılandırma dosyası | `mcpServers` | Yalnızca pencereyi değil uygulamayı tamamen kapatın | untested |
| Cline | `~/.cline/data/settings/cline_mcp_settings.json` | `mcpServers` | `disabled` ve `autoApprove` Cline'a özeldir | untested |
| Roo Code | Projede `.roo/mcp.json` veya VS Code global depolama ayarları | `mcpServers` | Ad çakışmasında proje girişi önceliklidir | untested |
| Kilo Code | `kilo.jsonc`, `.kilo/kilo.jsonc` veya `~/.config/kilo/kilo.jsonc` | `mcp` | OpenCode gibi tek komut dizisi | untested |
| Continue | `~/.continue/config.yaml` veya `.continue/mcpServers/` altındaki dosyalar | `mcpServers` | Eşleme değil YAML listesi; yalnızca Agent modu | untested |
| Zed | `~/.config/zed/settings.json` | `context_servers` | Anahtar adı `mcpServers` değildir | untested |
| JetBrains AI Assistant | Settings > Tools > AI Assistant > Model Context Protocol | `mcpServers` | Junie'den ayrı ayar | untested |
| Junie | Projede `.junie/mcp/mcp.json`, kullanıcıda `~/.junie/mcp/mcp.json` | `mcpServers` | Eski yazılarda `mcp/` klasörü olmayabilir | untested |
| Amp | `~/.config/amp/settings.json`, `.amp/settings.json` veya `amp mcp add` | `amp.mcpServers` | Anahtar `amp.` önekini korur | untested |
| Goose | `~/.config/goose/config.yaml` veya `%APPDATA%\Block\goose\config\config.yaml` | `extensions` | YAML; `command` değil `cmd` kullanır | untested |
| Kiro | `.kiro/settings/mcp.json` veya `~/.kiro/settings/mcp.json` | `mcpServers` | `${VAR}` genişletmesi onay gerektirir | untested |
| Antigravity | `~/.gemini/config/mcp_config.json` veya projede `.agents/mcp_config.json` | `mcpServers` | Uzak sunucular `serverUrl` kullanır | untested |
| OpenClaw | `~/.openclaw/openclaw.json` (JSON5) | `mcp.servers` | `transport: "stdio"` | untested |
| DeepSeek Harness (dsh) | `$DSH_HOME/cordis.patch.yml` veya profil yaması | `- insert:` | Her sunucu için bir eklenti örneği | untested |
| Warp | Ayarlar > Agents > MCP servers veya `.mcp.json` | `mcpServers` | `cwd` değil `working_directory` | untested |
| Augment Code | Auggie CLI için `~/.augment/settings.json` | `mcpServers` | IDE kendi panelini kullanır | untested |
| Qwen Code | `~/.qwen/settings.json` veya projede `.qwen/settings.json` | `mcpServers` | `mcp.allowed` sunucuyu gizleyebilir | untested |
| Kimi CLI | `~/.kimi-code/mcp.json` veya projede `.kimi-code/mcp.json` | `mcpServers` | Güncel Kimi Code CLI yolu | untested |
| Grok CLI | `~/.grok/config.toml` veya projede `.grok/config.toml` | `[mcp_servers.xerj]` | Grok Bot yerel stdio kaydını desteklemez | untested |

Codex ve Grok CLI TOML kullanır:

```toml
[mcp_servers.xerj]
command = "/abs/path/to/xerj"
args = ["mcp"]
```

OpenCode ve Kilo Code tek bir `command` dizisi kullanır; Kilo'da aynı yapı bir seviye daha basittir:

```json
{"mcp": {"servers": {"xerj": {"type": "local", "command": ["/abs/path/to/xerj", "mcp"], "environment": {"XERJ_URL": "http://localhost:9200"}}}}
```

Continue ve Goose YAML kullanır. Diğer istemcilerin yapılandırma örnekleri ile tam araç listesi [İngilizce README'de](./README.md#connect-an-agent-over-mcp) yer alır. Aider'ın MCP istemci desteği yoktur; T3 Code, başka ajanların bağlandığı MCP uç noktası sunar. Grok Bot için belgelenmiş yerel stdio kaydı yoktur; Grok CLI desteklenir.

XERJ yirmi araç sunar. Bunlardan bazıları Elasticsearch uyumlu uç noktaların ince birer aktarımıdır: `xerj_search`, `xerj_semantic_search`, `xerj_vector_search`, `xerj_hybrid_search`, `xerj_memory_store`, `xerj_memory_recall`, `xerj_brain_overview`, `xerj_brain_ego`, `xerj_brain_link`, `xerj_brain_unlink` ve `xerj_plan`. `xerj_plan`, `POST /_ask` üzerinden çalışan deterministik ve kural tabanlı sorgu planlayıcıdır; LLM değildir. Varsayılan anlamsal gömücü sinir ağı değil sözcüksel özellik karmasıdır; sinir ağı modu için düğüm `--embed-mode neural` ile başlatılmalıdır.

`xerj_search` aracında `{"prefix":"project","query":"retry policy"}` kullanmak `project-*` indekslerinde arama yapar. Belirli indeks adı veya kalıbı için `index` verin; `index` ile `prefix` aynı istekte kullanılamaz. İkisi de verilmezse `ax-*` aranır. `xerj_map`, autoindex kataloğundaki indeks alanlarını döndürür. `xerj_code_search`, `xerj code` referansla kodlama hattını çalıştırır. `xerj_index_budget`, klasörü sayıp indeksin disk bütçesini tahmin eder ancak indeksleme yapmaz. `xerj_autoindex_remove`, yalnızca onay verildiğinde indekslenmiş klasörü kaldırır. `xerj_corpus_feedback` Corpus Hub derlemini puanlar; `xerj_field_report` saha raporu gönderir; `xerj_hub_find`, `xerj_hub_def` ve `xerj_hub_ask` makinedeki Corpus Hub paketlerinde arama yapar.

`xerj mcp --help` aynı yapılandırma bloklarını ve seçeneklerin tamamını gösterir. Makinece okunabilir şemalar [burada](https://xerj.org/docs/agents/schemas/mcp-tools.json) yayımlanır; canlı `tools/list` yanıtından üretilir ve CI ile birim testi tarafından doğrulanır.

## Neden var?

Ajanlar dosyaları okurken bağlam pencerelerini tüketir. WordPress çekirdeğindeki PHP kodu yaklaşık 5,2 milyon token, yani 26 tam bağlam penceresi eder; bir ajan tümünü okuyamaz. Grep de tek başına çözüm değildir: eşleşen satırın ne anlama geldiğini anlamak için dosyanın tamamını açmak gerekir.

İndekse sorgu göndermek, soru başına kilobaytlarca veri gerektirir. [WordPress çekirdeğinde yapılan yapay zekâ güvenlik denetiminde](https://xerj.org/use-cases/code-security-audit.html) ajan 1.492 PHP dosyasında yaklaşık 26.000 tokenla çalıştı; bu, kod tabanının yaklaşık yüzde 0,5'ini bağlama almaya denktir.

## Kullanım alanları

- [Referansla kodlama](https://xerj.org/case-studies/reference-coding.html): Benzer projelerin çözümlerini yeniden keşfetmek yerine kodlama ajanınızla bulun.
- [Kod arama ve güvenlik denetimleri](https://xerj.org/use-cases/code-security-audit.html): AST farkındalıklı indeksleme ile tek bir satır yerine işlevi bulun.
- [Yapay zekâ araması ve RAG](https://xerj.org/use-cases/ai-search-retrieval.html): Ayrı vektör veritabanı olmadan tam metin, vektör ve hibrit erişim.
- [Ajan belleği](https://xerj.org/use-cases/second-brain.html): Kendi belgeleriniz üzerinde kalıcı hatırlama.
- [Günlük analizi ve gözlemlenebilirlik](https://xerj.org/use-cases/unified-observability.html): Günlükleri, ölçümleri ve izleri tek motorda birleştirin.
- [Elasticsearch alternatifi](https://xerj.org/use-cases/elasticsearch-replacement.html): Aynı ağ protokolü, tek ikili dosya.

Çalıştırılabilir örnekler [`recipes/`](./recipes) ve [`docs/examples/`](./docs/examples) altındadır.

## Elasticsearch uyumluluğu

XERJ; indeksler, belgeler, bulk, arama, toplulaştırmalar, eşlemeler, kNN, scroll, reindex ve `_cat` uç noktaları dâhil Elasticsearch REST API'sini uygular. Kibana ve resmî istemci kütüphaneleri doğrudan bağlanabilir. Dışa aktarma araçlarını kullanmadan önce bir sınıra dikkat edin: scroll, segmentler arasında ilerleyen bir imleç değil, baştan belirlenen boyutta bir anlık görüntüdür. İstenen toplam bu sınırı aşarsa sonuç sessizce kesilmez; istek `400` hatasıyla reddedilir. Her boyuttaki sonuç kümesi için `search_after` kullanın ([sınır](https://xerj.org/docs/api-es-compat.html#scroll-cap)).

Uyumluluk paketi her commit'te çalışır; README hazırlanırken 1369 durumun 1366'sı geçmektedir. Paket [`engine/tests/es-compat-yaml`](./engine/tests/es-compat-yaml) altındadır; kalan eksikler gizlenmek yerine orada listelenir. XERJ API ile uyumludur; Elasticsearch'ün iç işleyişini yeniden uygulayan bir proje veya Elasticsearch çatallaması değildir.

## Karşılaştırmalar

XERJ, veri alımı, tam metin araması, toplulaştırmalar, vektör araması ve eş zamanlı yoğun yazma sırasında okumalar dâhil Elasticsearch 8.13.4 ile karşılaştırılır. Son kapalı döngü çalışması 55 galibiyet, 26 beraberlik ve 4 mağlubiyet bildirmiştir; veri alımı 1,72 kat hızlı, diskteki alan kullanımı 1,61 kat daha küçüktür.

Dört kaybın tamamı, yüksek yazma hızında okuma p99 gecikmesiyle ilgilidir ve [ayrıntılı olarak yayımlanmıştır](./demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md). Sonuçlar, yöntem ve test aracı [xerj.org/benchmarks](https://xerj.org/benchmarks) ve [`demo/playbooks`](./demo/playbooks) altındadır; ölçümleri kendiniz tekrarlayabilirsiniz. Bizimkiler dâhil, yeniden üretemediğiniz sayılara şüpheyle yaklaşın.

### Kod indeksleme

Kaynak dosyalar tree-sitter ile ayrıştırılır ve AST farkındalıklı olarak (semboller ve tam metinle) indekslenir; böylece ajan tek bir satır değil, işlevi alır. Apache Lucene çalışma kopyasında (6.113 kaynak dosyası, 51 MB) tek iş parçacıklı süreç içi çıkarım saniyede yaklaşık 1.500 dosya (13 MB) işler. Tree-sitter ayrıştırması sürenin yaklaşık yüzde 72'sini alır ve dosya başına alt sınırı belirler. Ölçümü şu komutla tekrarlayın:

```sh
XERJ_EXTRACT_BENCH=/path/to/repo cargo test -p xerj-autoindex --release \
  extract_bench -- --nocapture --ignored
```

Yeniden indeksleme artımlıdır: içeriği son indekslenen sürümle bayt bayt aynı olan dosya tekrar ayrıştırılmaz, mevcut sonucu yeni sürüme taşınır. Birkaç dosya değiştiğinde yeniden çalıştırma tüm depoya değil değişen dosyalara göre iş yapar; yaygın düzenle-yeniden çalıştır veya CI senaryosunda dosyaların genellikle yüzde 95'ten fazlası değişmez. Değişiklikleri saptamak için tüm içeriğin özeti yine hesaplanır; bu nedenle toplam süreyi ayrıştırmadan çok özetleme belirleyebilir.

## Kaynaktan derleme

Stable Rust araç zinciri gerekir.

```sh
git clone https://github.com/xerj-org/xerj
cd xerj/engine
cargo build --release -p xerj-server
./target/release/xerj --insecure --data-dir ~/xerj-data
```

Çalışan sunucuya karşı uyumluluk paketini çalıştırmak için:

```sh
cargo run --release -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml
```

## Belgeler

- [Kılavuzlar ve API başvurusu](https://xerj.org/docs/)
- Yaygın işler için [tarifler](https://xerj.org/docs/recipes/)
- [Yol haritası](./ROADMAP.md): bugünkü özellikler ve sıradakiler. Ayrıca [kilometre taşları](https://github.com/xerj-org/xerj/milestones), [proje panosu](https://github.com/users/xerj-org/projects/1) ve sabitlenmiş [yol haritası konusu](https://github.com/xerj-org/xerj/issues/298).
- [Değişiklik günlüğü](./CHANGELOG.md)

Alt sistemler ve sınırları hakkında kaynak koda dayanan başvuru belgeleri:

- [Second brain](./docs/SECOND_BRAIN.md): indekslenmiş belgeler arasındaki bağlantılar, `/_graph` uç noktaları, kanıtlar, sekiz algılayıcı ve iki adım sınırı.
- [Konsol okuyucusu](./docs/CONSOLE_READER.md): derlem ana sayfası, e-posta ekleri ve PDF sayfaları dâhil kayıt okuyucusu, salt okunur misafir modu ve zararlı belgelerin etkisiz kalmasını sağlayan kurallar.
- [Betikleme](./docs/SCRIPTING.md): Painless alt kümesi, betiklerin çalıştığı yer ve kaynak sınırları.
- [Anlık görüntü ve geri yükleme](./docs/SNAPSHOT_AND_RESTORE.md): desteklenen API alt kümesi ve geri yüklemenin neleri değiştirdiği.
- [Paylaşım](./docs/SHARING.md): `xerj share` ile bağlantı ve parola üzerinden tek bir indekslenmiş klasöre salt okunur erişim, misafirin erişebildikleri ve tehdit modeli.
- [Güvenlik modeli](./docs/SECURITY_MODEL.md): kimlik doğrulama, ayrılmış `.xerj-memory-*` ad alanı, API anahtarları ve uygulanmayan kısıtlar.
- [Yeniden sıralama](./docs/RERANK.md): isteğe bağlı `rerank` arama adımı ve düğüm dışına belge metni gönderen bağlantılar.
- [XERJ ve Lucene 10.3.1 karşılaştırması](./docs/XERJ_VS_LUCENE.md): depolama ve arama tasarımlarının altı boyutlu, kaynak sürümleri sabitlenmiş karşılaştırması.

## Katkıda bulunma

İnsanlardan ve yapay zekâ ajanlarından gelen PR'lar açıktır; başlangıç katkısı, yukarıda anlatılan kısa saha raporudur. Kod katkıları için iş akışı [`CONTRIBUTING.md`](./CONTRIBUTING.md), katkı lisans sözleşmesi ve katkı inceleme yönergeleri [`CLA.md`](./CLA.md) içindedir. Bir katkıya yapay zekâ ajanı yardımcı olduysa PR açıklaması bunu belirtmeli ve doğrulananlarla varsayımları ayırmalıdır; ayrıntılar [`AGENTS.md`](./AGENTS.md) ve [`.github/AI_CONTRIBUTIONS.md`](./.github/AI_CONTRIBUTIONS.md) dosyalarındadır.

Hata bildirimleri ve özellik istekleri için [GitHub Issues](https://github.com/xerj-org/xerj/issues) kullanın.

## Lisans

[Apache 2.0](./LICENSE).
