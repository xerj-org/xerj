"""Agent usage stats on the Corpus Hub pages.

Numbers come from /api/hub-usage (functions/api/hub-usage.js) when the page
loads: requests to the hosted hub API (/api/v1/code|def|search) per corpus,
recorded as query templates by functions/_lib/hub-usage.js. The pages never
carry a number of their own. With no data, or with the recording not enabled,
they say so instead of showing a zero that looks measured.
"""

STYLE = """
/* agent usage */
.usage .kpis{margin:var(--sp-4) 0}
.usage h3{font-family:var(--font-data);font-size:var(--fs-11);font-weight:700;text-transform:uppercase;
letter-spacing:var(--track-label);color:var(--z-mute);margin:var(--sp-5) 0 var(--sp-2)}
.usage .topics span{display:inline-block;border:1px solid var(--z-line);padding:.2em .7em;margin:0 .4em .4em 0;font-family:var(--font-data);font-size:var(--fs-13)}
.usage table td.n{text-align:right;width:6em}
.usage [hidden],.mostused [hidden]{display:none}
.mostused ol{margin:var(--sp-3) 0 0;padding-left:1.4em}
.mostused li{margin:.2em 0;font-family:var(--font-data)}
/* traffic panel (GitHub Insights style) */
.traffic [hidden]{display:none}
.tgrid{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:1px;background:var(--z-line);border:1px solid var(--z-line);margin:var(--sp-4) 0}
.tchart{background:#fff;padding:var(--sp-4) var(--sp-5)}
.tchart h3{margin-top:0}
.tnums{font-family:var(--font-data);font-size:var(--fs-13);color:var(--z-mute);margin-bottom:var(--sp-2)}
.tnums b{font-family:var(--font-display);font-weight:800;font-size:var(--fs-20);color:var(--z-ink,#111)}
.tchart svg{display:block;width:100%;height:96px}
.tchart .bt{fill:#c4b5fd}.tchart .bu{fill:#6d28d9}.tchart .bn{fill:none;stroke:var(--z-line);stroke-dasharray:2 2}
.tchart .ax{font-family:var(--font-data);font-size:9px;fill:var(--z-mute)}
.tkey{font-family:var(--font-data);font-size:var(--fs-11);color:var(--z-mute)}
.tkey i{display:inline-block;width:.8em;height:.8em;margin:0 .3em 0 .8em;vertical-align:-.05em}
.tref{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:var(--sp-6)}
.tref td.n{text-align:right;width:6em}
"""

# textContent only: templates are user-derived text and are never parsed as HTML.
USAGE_JS = """<script>
(function(){
  var API='/api/hub-usage';
  function fmt(n){return Number(n||0).toLocaleString('en-US')}
  function el(t,txt,c){var e=document.createElement(t);if(txt!=null)e.textContent=txt;if(c)e.className=c;return e}
  var sec=document.querySelector('.usage[data-usage]');
  if(sec){
    var msg=sec.querySelector('[data-u-msg]'),body=sec.querySelector('[data-u-body]');
    fetch(API+'?corpus='+encodeURIComponent(sec.dataset.usage)).then(function(r){return r.json()}).then(function(r){
      if(!r||!r.ok){msg.textContent='Usage stats are unavailable right now.';return}
      if(!r.enabled){msg.textContent='Usage recording is not enabled on this deploy yet, so there is nothing to show.';return}
      if(!r.d30){msg.textContent='No usage recorded yet: no agent has asked the hosted hub about this corpus in the last 30 days.';return}
      msg.hidden=true;body.hidden=false;
      sec.querySelector('[data-u=d7]').textContent=fmt(r.d7);
      sec.querySelector('[data-u=d30]').textContent=fmt(r.d30);
      sec.querySelector('[data-u=agents]').textContent=fmt(r.agents30);
      var fam=Object.keys(r.agent_families||{}).filter(function(f){return f!=='unknown'}).sort(function(a,b){return r.agent_families[b]-r.agent_families[a]});
      sec.querySelector('[data-u=families]').textContent=fam.length?fam.map(function(f){return f+' '+fmt(r.agent_families[f])}).join(' · '):'none identified';
      var tp=sec.querySelector('[data-u-topics]'),tt=sec.querySelector('[data-u-templates]'),dn=sec.querySelector('[data-u-detailnote]');
      if(!r.detail){dn.textContent=!r.detail_requires_signin?'':(r.signin_enabled?'Sign in (top right) to see the topics and query templates.':'Topics and query templates are shown to signed-in visitors; hub sign-in is not switched on yet.');tp.parentNode.hidden=true;return}
      if(!r.detail.templates.length){dn.textContent='No query template has been seen '+r.k+' times yet, so none is shown.';tp.parentNode.hidden=true;return}
      r.detail.topics.forEach(function(t){tp.appendChild(el('span',t.term+' '+fmt(t.count)))});
      r.detail.templates.forEach(function(t){var tr=el('tr');tr.appendChild(el('td',t.template,'mono'));tr.appendChild(el('td',fmt(t.count),'mono n'));tt.appendChild(tr)});
    }).catch(function(){msg.textContent='Usage stats are unavailable right now.'});
  }
  var mu=document.querySelector('.mostused[data-usage-top]');
  if(mu)fetch(API).then(function(r){return r.json()}).then(function(r){
    var m=mu.querySelector('[data-u-msg]');
    if(!r||!r.ok){m.textContent='Usage stats are unavailable right now.';return}
    if(!r.enabled){m.textContent='Usage recording is not enabled on this deploy yet.';return}
    var top=(r.top||[]).filter(function(t){return t.d30>0}).slice(0,5);
    if(!top.length){m.textContent='No usage recorded yet: no agent has asked the hosted hub about any corpus in the last 30 days.';return}
    m.hidden=true;var ol=mu.querySelector('ol');ol.hidden=false;
    top.forEach(function(t){var li=el('li'),a=el('a',t.corpus);a.href='corpus/'+encodeURIComponent(t.corpus)+'.html';li.appendChild(a);
      li.appendChild(document.createTextNode(' \\u00b7 '+fmt(t.d30)+' requests in 30 days ('+fmt(t.d7)+' in 7)'));ol.appendChild(li)});
  }).catch(function(){});
  var tr=document.querySelector('.traffic[data-traffic]');
  if(tr){
    var tm=tr.querySelector('[data-t-msg]'),tb=tr.querySelector('[data-t-body]'),NS='http://www.w3.org/2000/svg';
    function svgEl(t,a){var e=document.createElementNS(NS,t);for(var k in a)e.setAttribute(k,a[k]);return e}
    function chart(svg,days,tot,uniq,label){
      var W=280,H=80,n=days.length,bw=W/n,max=1;
      days.forEach(function(d){if(d[tot]!=null&&d[tot]>max)max=d[tot]});
      svg.setAttribute('viewBox','0 0 '+W+' '+(H+14));svg.setAttribute('preserveAspectRatio','none');
      days.forEach(function(d,i){var x=i*bw+1,w=bw-2;
        if(d[tot]==null){svg.appendChild(svgEl('rect',{x:x,y:H-2,width:w,height:2,'class':'bn'}));return}
        var h=Math.round(H*d[tot]/max),g=svgEl('g',{}),t=svgEl('title',{});
        t.textContent=d.date+': '+fmt(d[tot])+' '+label+(uniq&&d[uniq]!=null?', '+fmt(d[uniq])+' unique':'');
        g.appendChild(t);g.appendChild(svgEl('rect',{x:x,y:H-h,width:w,height:Math.max(h,d[tot]?1:0),'class':'bt'}));
        if(uniq&&d[uniq]!=null){var hu=Math.round(H*d[uniq]/max);g.appendChild(svgEl('rect',{x:x,y:H-hu,width:w,height:Math.max(hu,d[uniq]?1:0),'class':'bu'}))}
        svg.appendChild(g)});
      [[0,'start'],[n-1,'end']].forEach(function(p){var tx=svgEl('text',{x:p[0]?W:0,y:H+12,'class':'ax','text-anchor':p[1]});tx.textContent=days[p[0]].date.slice(5);svg.appendChild(tx)});
    }
    fetch('/api/hub-traffic?corpus='+encodeURIComponent(tr.dataset.traffic)).then(function(r){return r.json()}).then(function(r){
      if(!r||!r.ok){tm.textContent='Traffic stats are unavailable right now.';return}
      if(!r.enabled){tm.textContent='Traffic counting is not enabled on this deploy yet, so there is nothing to show.';return}
      if(!r.recording_since&&!r.pulls_since){tm.textContent='No traffic recorded yet: counting has not started on this hub.';return}
      tm.hidden=true;tb.hidden=false;var T=r.totals;
      function put(k,v){var e=tr.querySelector('[data-t='+k+']');if(e)e.textContent=v==null?'not recorded yet':fmt(v)}
      put('views',T.views);put('unique_views',T.unique_views);put('pulls',T.pulls);put('searches',T.searches);
      chart(tr.querySelector('[data-chart=views] svg'),r.days,'views','unique_views','views');
      chart(tr.querySelector('[data-chart=pulls] svg'),r.days,'pulls',null,'pulls');
      chart(tr.querySelector('[data-chart=searches] svg'),r.days,'searches',null,'searches');
      var since=tr.querySelector('[data-t-since]');
      since.textContent=(r.recording_since?'Views and searches counted since '+r.recording_since+'. ':'')+(r.pulls_since?'Pulls counted since '+r.pulls_since+'.':'Pulls: the first daily snapshot of the pack download counts has not been taken yet.');
      function rows(sel,list,key,empty){var t=tr.querySelector(sel);if(!list.length){var e=el('tr');e.appendChild(el('td',empty,'note'));t.appendChild(e);return}
        list.slice(0,10).forEach(function(x){var e=el('tr');e.appendChild(el('td',x[key],'mono'));e.appendChild(el('td',fmt(x.count),'mono n'));t.appendChild(e)})}
      rows('[data-t-agents]',r.agents||[],'family','No agent identified yet.');
      rows('[data-t-refs]',r.referrers||[],'host','No referring site has sent '+r.k+' views yet.');
    }).catch(function(){tm.textContent='Traffic stats are unavailable right now.'});
  }
})();
</script>"""

WHAT_COUNTS = ("Counted: requests agents send to the hosted hub API "
               "(<code>hub.xerj.org/api/v1/code</code>, <code>/def</code>) for this corpus. "
               "Queries on your own node (<code>xerj code</code>, <code>xerj ask</code> with a local pack, "
               "the MCP tools) never leave your machine and are not counted. Templates replace identifiers, "
               "versions, paths, numbers and quoted text with placeholders; one is shown only once it has been "
               "seen 3 times in 30 days, and any word asked fewer than 3 times shows as <code>&lt;term&gt;</code>. "
               "Raw queries are never published.")


def corpus_section(slug, esc, rel="../"):
    s = esc(slug.lower())
    return f'''<section class="detail usage" id="usage" data-usage="{s}"><h2>Agent usage</h2>
<p class="note" data-u-msg>Usage stats load from <code>/api/hub-usage</code> when this page runs JavaScript.</p>
<div data-u-body hidden>
<div class="kpis"><div class="kpi"><b data-u="d7">&ndash;</b><span>requests, last 7 days</span></div>
<div class="kpi"><b data-u="d30">&ndash;</b><span>requests, last 30 days</span></div>
<div class="kpi"><b data-u="agents">&ndash;</b><span>agent families, 30 days</span></div></div>
<p class="note">by user-agent family: <span data-u="families"></span></p>
<p class="note" data-u-detailnote></p>
<div><h3>Top topics</h3><p class="topics" data-u-topics></p>
<h3>Top query templates</h3><table data-u-templates><tr><th>template</th><th>requests</th></tr></table></div>
</div>
<p class="note">{WHAT_COUNTS} <a href="{rel}whats-this/#privacy">What is collected</a>.</p></section>'''


def traffic_section(slug, esc):
    s = esc(slug.lower())
    def tile(key, title, nums):
        return (f'<div class="tchart" data-chart="{key}"><h3>{title}</h3><div class="tnums">{nums}</div>'
                f'<svg role="img" aria-label="{title}, last 14 days"></svg></div>')
    tiles = "\n".join([
        tile("views", "Views", '<b data-t="views">&ndash;</b> total &middot; <b data-t="unique_views">&ndash;</b> unique'),
        tile("pulls", "Pulls", '<b data-t="pulls">&ndash;</b> total &middot; unique not reported by GitHub'),
        tile("searches", "Searches", '<b data-t="searches">&ndash;</b> hosted-hub queries')])
    return f'''<section class="detail traffic" id="traffic" data-traffic="{s}"><h2>Traffic</h2>
<p class="note" data-t-msg>Traffic stats load from <code>/api/hub-traffic</code> when this page runs JavaScript.</p>
<div data-t-body hidden>
<p class="tkey">last 14 days, UTC<i class="bt" style="background:#c4b5fd"></i>total<i style="background:#6d28d9"></i>unique</p>
<div class="tgrid">
{tiles}
</div>
<div class="tref"><div><h3>Agents</h3><table data-t-agents><tr><th>user-agent family</th><th>requests</th></tr></table></div>
<div><h3>Referring sites</h3><table data-t-refs><tr><th>site</th><th>views</th></tr></table></div></div>
<p class="note" data-t-since></p>
</div>
<p class="note">Views: visits to this page (search-engine crawlers excluded). Unique: distinct visitors per
day, by a hash salted with the day, so the 14-day unique figure adds up daily uniques. Pulls: growth of the
GitHub release download count of this corpus&rsquo;s pack (<code>xerj hub pull</code> downloads from
there; includes our own pack-verification downloads). Searches: hosted-hub queries for this corpus.
Referring sites appear after 3 views. <a href="../whats-this/#privacy">What is collected</a>.</p></section>'''


def home_section():
    return '''<section class="detail mostused" data-usage-top aria-label="Most used corpora"><h2>Most used corpora</h2>
<p class="note" data-u-msg>Usage stats load from <code>/api/hub-usage</code> when this page runs JavaScript.</p>
<ol hidden></ol>
<p class="note">Requests agents sent to the hosted hub API in the last 30 days. Local queries are not counted.
<a href="whats-this/#privacy">What is collected</a>.</p></section>'''


PRIVACY = '''<section class="detail" id="privacy"><h2>Privacy: what the hub collects</h2>
<p><b>Agent usage.</b> When an agent sends a request to the hosted hub API
(<code>/api/v1/code</code>, <code>/api/v1/def</code>, <code>/api/v1/search</code>), the hub stores
three things: the corpus and route, a <em>template</em> of the query, and the coarse
user-agent family (for example <code>xerj-cli</code>, <code>curl</code>, <code>python</code>).
The template already has identifiers, versions, paths, numbers, quoted strings, URLs,
e-mail addresses and IP addresses replaced by placeholders. The hub does <b>not</b> store
the raw query, your IP address, the full user agent, cookies or any user id. Records
are deleted after 40 days. Pages show only totals, and show a template only once it has
been seen 3 times in 30 days, with every word asked fewer than 3 times generalised to
<code>&lt;term&gt;</code>. To opt out of being counted, send <code>DNT: 1</code>,
<code>Sec-GPC: 1</code> or <code>X-Xerj-Disable-Feedback: 1</code>. Queries you run on your
own node (<code>xerj code</code>, <code>xerj ask</code> with a local pack, the MCP tools) never reach
the hub. The xerj client sends no query telemetry.</p>
<p><b>Corpus page traffic.</b> A visit to a corpus page stores a visitor hash that changes every
day (a salted hash of your IPv4 address or IPv6 /64 and the date, so two days cannot be linked), the
user-agent family and the referring site&rsquo;s host name, never the full referring URL. Search-engine
crawlers are not counted, and the same opt-out headers apply. Pull counts come from GitHub&rsquo;s public
release download counts, not from you.</p>
<p><b>Stars.</b> A hub star stores a salted hash of your IPv4 address or IPv6 /64, so one
source counts once. The raw address is never stored.</p></section>'''
